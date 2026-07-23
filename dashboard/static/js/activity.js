var activityLog = [];
var activityEnabled = false;
var activityLevel = 'standard';
var activityCaptureState = 'off';
var activityStatus = null;
var activityFilters = {
    all: true,
    announce: true,
    path: true,
    message: true,
    lxst: true,
    interface: true,
    link: true,
    error: true
};

var ACTIVITY_MAX_ENTRIES = 500;
var _activityRenderScheduled = false;
var _activityControlToken = 0;
var _activityControlPending = false;

// Auto-scroll new entries only when pinned to bottom; 8px tolerance for sub-pixel rounding.
var activityStickToBottom = true;
var ACTIVITY_STICK_TOLERANCE_PX = 8;

var ACTIVITY_FILTER_LABELS = {
    all: 'All',
    announce: 'Announces',
    path: 'Paths',
    message: 'Messages',
    lxst: 'LXST',
    interface: 'Interfaces',
    link: 'Links',
    error: 'Errors'
};

// essential < standard < detailed
var LEVEL_HIERARCHY = { essential: 0, standard: 1, detailed: 2 };

var LEVEL_TYPES = {
    essential: ['error'],
    standard: ['error', 'message', 'lxst', 'interface', 'link'],
    detailed: ['error', 'message', 'lxst', 'interface', 'link', 'announce', 'path']
};

var ACTIVITY_U64_MAX = '18446744073709551615';
var ACTIVITY_REPLAY_MAX_EVENTS = 50;
var ACTIVITY_REPLAY_MAX_BYTES = 65536;
var ACTIVITY_LISTENER_RETRY_DELAYS = [100, 200, 400, 800, 1600, 2000];
var activityLegacyGenerationFloor = null;
var activityLegacyBlocked = false;

function activityIsCanonicalU64(value, allowZero) {
    if (typeof value !== 'string' || !/^(0|[1-9][0-9]*)$/.test(value)) return false;
    if (!allowZero && value === '0') return false;
    if (value.length !== ACTIVITY_U64_MAX.length) return value.length < ACTIVITY_U64_MAX.length;
    return value <= ACTIVITY_U64_MAX;
}

function activityCompareU64(left, right) {
    if (!activityIsCanonicalU64(left, true) || !activityIsCanonicalU64(right, true)) return null;
    if (left.length !== right.length) return left.length < right.length ? -1 : 1;
    if (left === right) return 0;
    return left < right ? -1 : 1;
}

function activityIncrementU64(value) {
    if (!activityIsCanonicalU64(value, true) || value === ACTIVITY_U64_MAX) return null;
    var digits = value.split('');
    for (var i = digits.length - 1; i >= 0; i--) {
        var code = digits[i].charCodeAt(0);
        if (code < 57) {
            digits[i] = String.fromCharCode(code + 1);
            return digits.join('');
        }
        digits[i] = '0';
    }
    return '1' + digits.join('');
}

function activityAdvanceLegacyGenerationFloor(generation) {
    if (!activityIsCanonicalU64(generation, true)) return false;
    if (activityLegacyGenerationFloor == null
        || activityCompareU64(generation, activityLegacyGenerationFloor) > 0) {
        activityLegacyGenerationFloor = generation;
    }
    return true;
}

function activityLegacyEventAllowed(entry) {
    if (activityLegacyBlocked) return false;
    if (!entry || !activityIsCanonicalU64(entry.capture_generation, true)) return false;
    return activityLegacyGenerationFloor == null
        || activityCompareU64(entry.capture_generation, activityLegacyGenerationFloor) >= 0;
}

function activityPruneLegacyBefore(generation) {
    if (!activityAdvanceLegacyGenerationFloor(generation)) return;
    activityLog = activityLog.filter(activityLegacyEventAllowed);
}

function activitySequenceSort(left, right) {
    return activityCompareU64(left.sequence, right.sequence);
}

function activityValidEvent(event, captureSession) {
    if (!event || typeof event !== 'object' || event.version !== 1) return false;
    if (!activityIsCanonicalU64(event.sequence, false)) return false;
    if (!activityIsCanonicalU64(event.capture_generation, true)) return false;
    if (event.parent_sequence != null && !activityIsCanonicalU64(event.parent_sequence, false)) return false;
    if (typeof event.capture_session !== 'string' || !event.capture_session) return false;
    return !captureSession || event.capture_session === captureSession;
}

function activityValidStatus(status) {
    if (!status || typeof status !== 'object' || status.version !== 1) return false;
    if (['off', 'capturing', 'stopped'].indexOf(status.state) === -1) return false;
    if (!activityIsCanonicalU64(status.ingress_generation, true)) return false;
    if (!activityIsCanonicalU64(status.worker_epoch, true)) return false;
    if (status.oldest != null && !activityIsCanonicalU64(status.oldest, false)) return false;
    if (status.latest != null && !activityIsCanonicalU64(status.latest, false)) return false;
    if ((status.oldest == null) !== (status.latest == null)) return false;
    if (status.oldest != null && activityCompareU64(status.oldest, status.latest) > 0) return false;
    if (status.capture_session != null && (typeof status.capture_session !== 'string' || !status.capture_session)) return false;
    if (status.state !== 'off' && !status.capture_session) return false;
    if (status.state === 'off' && status.profile != null) return false;
    if (status.state !== 'off' && ['normal', 'trace'].indexOf(status.profile) === -1) return false;
    return true;
}

function activityStatusIsOlder(candidate, currentStatus) {
    if (!currentStatus) return false;
    var workerComparison = activityCompareU64(candidate.worker_epoch, currentStatus.worker_epoch);
    if (workerComparison !== 0) return workerComparison < 0;
    var generationComparison = activityCompareU64(
        candidate.ingress_generation,
        currentStatus.ingress_generation
    );
    if (generationComparison !== 0) return generationComparison < 0;
    if (candidate.latest == null) return currentStatus.latest != null;
    if (currentStatus.latest == null) return false;
    return activityCompareU64(candidate.latest, currentStatus.latest) < 0;
}

function activityValidateBatch(payload) {
    if (!payload || typeof payload !== 'object' || payload.version !== 1) return null;
    if (typeof payload.capture_session !== 'string' || !payload.capture_session) return null;
    if (!activityIsCanonicalU64(payload.first_sequence, false)) return null;
    if (!activityIsCanonicalU64(payload.last_sequence, false)) return null;
    if (!Array.isArray(payload.events) || payload.events.length === 0) return null;
    var events = payload.events.slice().sort(activitySequenceSort);
    for (var i = 0; i < events.length; i++) {
        if (!activityValidEvent(events[i], payload.capture_session)) return null;
    }
    if (events[0].sequence !== payload.first_sequence) return null;
    if (events[events.length - 1].sequence !== payload.last_sequence) return null;
    return { captureSession: payload.capture_session, events: events };
}

function activityMergeEvents(left, right, maxEvents) {
    var bySequence = Object.create(null);
    var conflict = false;
    var all = (left || []).concat(right || []);
    for (var i = 0; i < all.length; i++) {
        var event = all[i];
        if (!activityValidEvent(event)) {
            conflict = true;
            continue;
        }
        var prior = bySequence[event.sequence];
        if (prior && activityStableJson(prior) !== activityStableJson(event)) conflict = true;
        if (!prior) bySequence[event.sequence] = event;
    }
    var merged = Object.keys(bySequence).map(function(sequence) {
        return bySequence[sequence];
    }).sort(activitySequenceSort);
    if (merged.length > maxEvents) merged = merged.slice(merged.length - maxEvents);
    return { events: merged, conflict: conflict };
}

function activityStableJson(value) {
    if (value == null || typeof value !== 'object') return JSON.stringify(value);
    if (Array.isArray(value)) {
        return '[' + value.map(activityStableJson).join(',') + ']';
    }
    var keys = Object.keys(value).sort();
    return '{' + keys.map(function(key) {
        return JSON.stringify(key) + ':' + activityStableJson(value[key]);
    }).join(',') + '}';
}

function activityEventsAreContiguous(events) {
    for (var i = 1; i < events.length; i++) {
        if (activityIncrementU64(events[i - 1].sequence) !== events[i].sequence) return false;
    }
    return true;
}

function createActivityBootstrap(dependencies) {
    var deps = dependencies || {};
    var listen = deps.listen || function(name, handler, options) { return RS.listen(name, handler, options); };
    var invoke = deps.invoke || function(name, args) { return RS.invoke(name, args); };
    var schedule = deps.setTimeout || function(handler, delay) { return setTimeout(handler, delay); };
    var cancelSchedule = deps.clearTimeout || function(timer) { clearTimeout(timer); };
    var retryDelays = deps.retryDelays || ACTIVITY_LISTENER_RETRY_DELAYS;
    var mobileRuntime = (typeof isTauriMobile === 'function' && isTauriMobile())
        || (typeof isMobile === 'function' && isMobile());
    var maxEvents = deps.maxEvents || (mobileRuntime ? 2000 : 5000);
    var onStatus = deps.onStatus || function() {};
    var onIdentityTransition = deps.onIdentityTransition || function() {};
    var onPrivacyBoundary = deps.onPrivacyBoundary || function() {};
    var onLegacyClear = deps.onLegacyClear || function() {};
    var diagnose = deps.diagnose || function() {
        if (window.RS && typeof window.RS.diag === 'function') {
            var args = Array.prototype.slice.call(arguments);
            args.unshift('warn');
            window.RS.diag.apply(window.RS, args);
        }
    };
    var state = {
        phase: 'cold',
        epoch: 0,
        captureSession: null,
        status: null,
        statusFence: null,
        events: [],
        queuedLive: [],
        queueOverflow: false,
        queueInvalid: false,
        unlisteners: [],
        listenersAttached: false,
        retryTimer: null,
        pendingResync: false,
        recoveryAttempts: 0,
        identityGeneration: null,
        identityQuarantine: false,
        lastReason: null
    };

    function current(epoch) {
        return state.epoch === epoch;
    }

    function transition(phase, reason) {
        state.phase = phase;
        if (reason) state.lastReason = reason;
    }

    function nextEpoch(phase, reason) {
        state.epoch += 1;
        transition(phase, reason);
        return state.epoch;
    }

    function cleanupInstalled(installed) {
        (installed || []).forEach(function(unlisten) {
            if (typeof unlisten === 'function') {
                try { unlisten(); } catch (_) {}
            }
        });
    }

    function fail(epoch, reason, error) {
        if (!current(epoch)) return;
        transition('degraded', reason);
        diagnose('[Activity]', reason, error && error.code ? error.code : 'error');
    }

    function queueBatch(batch) {
        for (var i = 0; i < batch.events.length; i++) {
            state.queuedLive.push(batch.events[i]);
        }
        if (state.queuedLive.length > maxEvents) {
            state.queuedLive.splice(0, state.queuedLive.length - maxEvents);
            state.queueOverflow = true;
        }
    }

    function drainQueue(captureSession) {
        var accepted = state.queuedLive.filter(function(event) {
            return event.capture_session === captureSession;
        });
        state.queuedLive = [];
        return accepted;
    }

    function pruneBatchToKnownOldest(batch) {
        var oldest = state.status && state.status.capture_session === batch.captureSession
            ? state.status.oldest
            : null;
        if (oldest == null) return batch;
        batch.events = batch.events.filter(function(event) {
            return activityCompareU64(event.sequence, oldest) >= 0;
        });
        return batch;
    }

    function beginIdentityTransition(payload) {
        var mustReattach = !state.listenersAttached;
        if (state.retryTimer) cancelSchedule(state.retryTimer);
        state.retryTimer = null;
        nextEpoch('identity_transition', 'identity_switching');
        state.captureSession = null;
        state.status = null;
        state.statusFence = null;
        state.events = [];
        state.queuedLive = [];
        state.queueOverflow = false;
        state.queueInvalid = false;
        state.pendingResync = false;
        state.recoveryAttempts = 0;
        state.identityGeneration = payload
            && activityIsCanonicalU64(payload.generation, true)
            ? payload.generation
            : null;
        state.identityQuarantine = true;
        activityLegacyBlocked = true;
        onIdentityTransition();
        if (mustReattach) {
            state.retryTimer = schedule(function() {
                state.retryTimer = null;
                start();
            }, 0);
        }
    }

    function handleBatch(payload) {
        if (state.identityQuarantine) return;
        var batch = activityValidateBatch(payload);
        if (!batch) {
            state.queueInvalid = true;
            if (state.listenersAttached && state.phase !== 'subscribing' && state.phase !== 'identity_transition') {
                requestResync('invalid_batch', true);
            }
            return;
        }
        batch = pruneBatchToKnownOldest(batch);
        if (!batch.events.length) return;
        if (state.phase === 'live' && !state.captureSession) {
            requestResync('batch_without_session', true);
            return;
        }
        if (state.captureSession && batch.captureSession !== state.captureSession) {
            requestResync('batch_session_mismatch', true);
            return;
        }
        if (state.phase !== 'live') {
            queueBatch(batch);
            return;
        }
        var merged = activityMergeEvents(state.events, batch.events, maxEvents);
        state.events = merged.events;
        if (merged.conflict || !activityEventsAreContiguous(state.events)) {
            requestResync(merged.conflict ? 'conflicting_duplicate' : 'live_sequence_gap', true);
        }
    }

    function handleStatusNotification(status) {
        // An ordinary status has no identity-generation binding. Quarantine it
        // for the full switch span so a delayed old Capturing notification
        // cannot reopen legacy delivery before authoritative reconciliation.
        if (state.identityQuarantine) return;
        if (!activityValidStatus(status)) {
            requestResync('invalid_status_notification', true);
            return;
        }
        if (activityStatusIsOlder(status, state.statusFence)) return;
        state.statusFence = status;
        if (status.capture_session == null) {
            state.events = [];
            state.queuedLive = [];
            state.captureSession = null;
            onPrivacyBoundary(status, 'status_off');
        } else if (state.captureSession === status.capture_session) {
            state.events = pruneBeforeOldest(state.events, status.oldest);
            state.queuedLive = pruneBeforeOldest(state.queuedLive, status.oldest);
        }
        state.status = status;
        onStatus(status);
        requestResync('status_notification', true);
    }

    function handleBoundary(payload) {
        if (!payload || payload.version !== 1 || payload.kind !== 'hard_reset'
            || !activityIsCanonicalU64(payload.identity_generation, true)
            || !activityIsCanonicalU64(payload.capture_generation, true)
            || !activityValidStatus(payload.status)
            || payload.status.state !== 'off'
            || payload.status.capture_session != null
            || payload.capture_generation !== payload.status.ingress_generation) {
            requestResync('invalid_boundary_notification', true);
            return;
        }
        if (state.phase === 'identity_transition') {
            if (state.identityGeneration == null
                || payload.identity_generation !== state.identityGeneration) {
                return;
            }
            state.statusFence = payload.status;
            state.status = payload.status;
            state.captureSession = null;
            state.events = [];
            state.queuedLive = [];
            onPrivacyBoundary(payload.status, 'identity_hard_reset');
            return;
        }
        handleStatusNotification(payload.status);
    }

    function finishIdentityTransition(payload, reason) {
        if (state.phase === 'identity_transition' && state.identityGeneration != null) {
            if (!payload || payload.generation !== state.identityGeneration) return;
        }
        state.identityGeneration = null;
        requestResync(reason, true);
    }

    function handleLegacyClear(payload) {
        if (state.identityQuarantine) return;
        if (!payload || payload.version !== 1
            || !activityIsCanonicalU64(payload.capture_generation, true)) {
            requestResync('invalid_legacy_clear_notification', true);
            return;
        }
        onLegacyClear(payload.capture_generation);
        requestResync('legacy_clear_notification', true);
    }

    var listenerSpecs = [
        ['identity_switching', beginIdentityTransition],
        ['identity_switched', function(payload) {
            finishIdentityTransition(payload, 'identity_switched');
        }],
        ['identity_error', function() {
            state.identityGeneration = null;
            requestResync('identity_error', true);
        }],
        ['activity_status_v1', handleStatusNotification],
        ['activity_boundary_v1', handleBoundary],
        ['activity_legacy_cleared_v1', handleLegacyClear],
        ['activity_batch_v1', handleBatch]
    ];

    function attachListenerAt(epoch, index, installed) {
        if (!current(epoch)) return Promise.resolve();
        if (index >= listenerSpecs.length) return Promise.resolve(installed);
        var spec = listenerSpecs[index];
        return Promise.resolve().then(function() {
            return listen(spec[0], spec[1], { required: true });
        }).then(function(unlisten) {
            if (!current(epoch)) {
                cleanupInstalled([unlisten]);
                return installed;
            }
            installed.push(unlisten);
            return attachListenerAt(epoch, index + 1, installed);
        });
    }

    function subscribeAttempt(epoch, attempt) {
        if (!current(epoch)) return;
        transition('subscribing', 'listener_attempt');
        var installed = [];
        attachListenerAt(epoch, 0, installed).then(function(attached) {
            if (!current(epoch)) {
                cleanupInstalled(attached);
                return;
            }
            state.unlisteners = attached;
            state.listenersAttached = true;
            beginStatus(epoch);
        }).catch(function(error) {
            cleanupInstalled(installed);
            if (!current(epoch)) return;
            if (attempt < retryDelays.length) {
                state.retryTimer = schedule(function() {
                    state.retryTimer = null;
                    subscribeAttempt(epoch, attempt + 1);
                }, retryDelays[attempt]);
                return;
            }
            fail(epoch, 'listener_unavailable', error);
        });
    }

    function validateReplayPage(page, captureSession) {
        if (!page || typeof page !== 'object' || page.version !== 1) return null;
        if (page.capture_session !== captureSession || !Array.isArray(page.events)) return null;
        if (page.oldest != null && !activityIsCanonicalU64(page.oldest, false)) return null;
        if (page.latest != null && !activityIsCanonicalU64(page.latest, false)) return null;
        if (page.next_after != null && !activityIsCanonicalU64(page.next_after, false)) return null;
        if ((page.oldest == null) !== (page.latest == null)) return null;
        if (page.oldest != null && activityCompareU64(page.oldest, page.latest) > 0) return null;
        if (typeof page.has_more !== 'boolean' || typeof page.gap !== 'boolean') return null;
        var events = [];
        for (var i = 0; i < page.events.length; i++) {
            if (!activityValidEvent(page.events[i], captureSession)) return null;
            if (page.oldest != null && activityCompareU64(page.events[i].sequence, page.oldest) < 0) return null;
            if (page.latest != null && activityCompareU64(page.events[i].sequence, page.latest) > 0) return null;
            events.push(page.events[i]);
        }
        return {
            events: events,
            nextAfter: page.next_after,
            hasMore: page.has_more,
            gap: page.gap
        };
    }

    function replayUntil(epoch, status, collector, after, fence, pageCount, done) {
        if (!current(epoch)) return;
        transition('replaying', 'replay');
        invoke('activity_replay', {
            args: {
                capture_session: status.capture_session,
                after: after,
                max_events: ACTIVITY_REPLAY_MAX_EVENTS,
                max_bytes: ACTIVITY_REPLAY_MAX_BYTES
            }
        }).then(function(response) {
            if (!current(epoch)) return;
            if (response && response.result === 'session_mismatch') {
                requestResync('replay_session_mismatch', true);
                return;
            }
            if (!response || response.result !== 'page') {
                requestRecovery('invalid_replay_response');
                return;
            }
            var page = validateReplayPage(response.page, status.capture_session);
            if (!page) {
                requestRecovery('invalid_replay_page');
                return;
            }
            if (page.gap) {
                requestRecovery('replay_gap');
                return;
            }
            var merged = activityMergeEvents(collector, page.events, maxEvents);
            if (merged.conflict) {
                requestRecovery('conflicting_replay_duplicate');
                return;
            }
            collector = merged.events;
            var highest = collector.length ? collector[collector.length - 1].sequence : null;
            if (fence == null || (highest != null && activityCompareU64(highest, fence) >= 0)) {
                done(collector);
                return;
            }
            if (!page.hasMore) {
                done(collector);
                return;
            }
            if (!page.nextAfter || (after != null && activityCompareU64(page.nextAfter, after) <= 0)) {
                requestRecovery('replay_cursor_stalled');
                return;
            }
            if (pageCount >= maxEvents + 5) {
                requestRecovery('replay_page_limit');
                return;
            }
            replayUntil(epoch, status, collector, page.nextAfter, fence, pageCount + 1, done);
        }).catch(function(error) {
            if (current(epoch)) fail(epoch, 'replay_failed', error);
        });
    }

    function pruneBeforeOldest(events, oldest) {
        if (oldest == null) return events;
        return events.filter(function(event) {
            return activityCompareU64(event.sequence, oldest) >= 0;
        });
    }

    function statusRangeIsPresent(events, status) {
        if (!activityEventsAreContiguous(events)) return false;
        if (status.latest == null) return true;
        if (!events.length || status.oldest == null) return false;
        if (events[0].sequence !== status.oldest) return false;
        return activityCompareU64(events[events.length - 1].sequence, status.latest) >= 0;
    }

    function finalizeLive(epoch, status, collected) {
        if (!current(epoch)) return;
        var merged = activityMergeEvents(collected, drainQueue(status.capture_session), maxEvents);
        if (merged.conflict || state.queueOverflow || state.queueInvalid) {
            requestRecovery(merged.conflict ? 'conflicting_queued_duplicate' : 'queued_live_loss');
            return;
        }
        var events = pruneBeforeOldest(merged.events, status.oldest);
        if (!statusRangeIsPresent(events, status)) {
            requestRecovery('missing_sequence');
            return;
        }
        state.events = events;
        state.status = status;
        state.captureSession = status.capture_session;
        state.queueOverflow = false;
        state.queueInvalid = false;
        state.recoveryAttempts = 0;
        transition('live', 'reconciled');
        if (state.pendingResync) {
            state.pendingResync = false;
            requestResync('pending_resync', true);
        }
    }

    function reconcile(epoch, initialStatus, collected) {
        if (!current(epoch)) return;
        transition('reconciling', 'final_status');
        var firstMerge = activityMergeEvents(collected, drainQueue(initialStatus.capture_session), maxEvents);
        if (firstMerge.conflict || state.queueOverflow || state.queueInvalid) {
            requestRecovery(firstMerge.conflict ? 'conflicting_queued_duplicate' : 'queued_live_loss');
            return;
        }
        invoke('activity_status').then(function(finalStatus) {
            if (!current(epoch)) return;
            if (!activityValidStatus(finalStatus)) {
                requestRecovery('invalid_final_status');
                return;
            }
            if (activityStatusIsOlder(finalStatus, state.statusFence)) {
                requestResync('stale_final_status', true);
                return;
            }
            state.statusFence = finalStatus;
            onStatus(finalStatus);
            if (finalStatus.capture_session !== initialStatus.capture_session) {
                requestResync('final_status_session_mismatch', true);
                return;
            }
            state.status = finalStatus;
            var events = pruneBeforeOldest(firstMerge.events, finalStatus.oldest);
            var queuedMerge = activityMergeEvents(events, drainQueue(finalStatus.capture_session), maxEvents);
            if (queuedMerge.conflict || state.queueOverflow || state.queueInvalid) {
                requestRecovery(queuedMerge.conflict ? 'conflicting_queued_duplicate' : 'queued_live_loss');
                return;
            }
            events = pruneBeforeOldest(queuedMerge.events, finalStatus.oldest);
            var highest = events.length ? events[events.length - 1].sequence : null;
            if (finalStatus.latest != null && (highest == null || activityCompareU64(highest, finalStatus.latest) < 0)) {
                replayUntil(epoch, finalStatus, events, highest, finalStatus.latest, 0, function(caughtUp) {
                    finalizeLive(epoch, finalStatus, caughtUp);
                });
                return;
            }
            finalizeLive(epoch, finalStatus, events);
        }).catch(function(error) {
            if (current(epoch)) fail(epoch, 'final_status_failed', error);
        });
    }

    function acceptStatus(epoch, status, authoritative) {
        if (!current(epoch)) return;
        if (!activityValidStatus(status)) {
            fail(epoch, 'invalid_status');
            return;
        }
        if (activityStatusIsOlder(status, state.statusFence)) {
            requestResync('stale_initial_status', true);
            return;
        }
        if (authoritative && state.identityQuarantine) {
            state.identityQuarantine = false;
        }
        state.statusFence = status;
        state.status = status;
        state.captureSession = status.capture_session;
        onStatus(status);
        if (!status.capture_session || status.latest == null) {
            reconcile(epoch, status, []);
            return;
        }
        var initialLatestFence = status.latest;
        replayUntil(epoch, status, [], null, initialLatestFence, 0, function(events) {
            reconcile(epoch, status, events);
        });
    }

    function beginStatus(epoch) {
        if (!current(epoch)) return;
        transition('status', 'status');
        invoke('activity_status').then(function(status) {
            acceptStatus(epoch, status, true);
        }).catch(function(error) {
            if (current(epoch)) fail(epoch, 'status_failed', error);
        });
    }

    function requestRecovery(reason) {
        state.recoveryAttempts += 1;
        if (state.recoveryAttempts > 4) {
            fail(state.epoch, reason);
            return;
        }
        requestResync(reason, true);
    }

    function requestResync(reason, force) {
        if (!state.listenersAttached) {
            if (state.phase !== 'subscribing') start();
            return;
        }
        if (!force && ['status', 'replaying', 'reconciling', 'resyncing'].indexOf(state.phase) !== -1) {
            state.pendingResync = true;
            return;
        }
        var epoch = nextEpoch('resyncing', reason || 'resync');
        state.events = [];
        state.queuedLive = [];
        state.queueOverflow = false;
        state.queueInvalid = false;
        state.captureSession = null;
        state.status = null;
        state.pendingResync = false;
        beginStatus(epoch);
    }

    function start() {
        if (state.listenersAttached) {
            requestResync('start', false);
            return;
        }
        if (state.retryTimer) cancelSchedule(state.retryTimer);
        state.retryTimer = null;
        var epoch = nextEpoch('subscribing', 'start');
        state.queuedLive = [];
        state.queueOverflow = false;
        state.queueInvalid = false;
        state.pendingResync = false;
        subscribeAttempt(epoch, 0);
    }

    function snapshot() {
        return {
            phase: state.phase,
            epoch: state.epoch,
            captureSession: state.captureSession,
            status: state.status,
            statusFence: state.statusFence,
            events: state.events.slice(),
            queuedLiveCount: state.queuedLive.length,
            queueOverflow: state.queueOverflow,
            listenersAttached: state.listenersAttached,
            identityGeneration: state.identityGeneration,
            identityQuarantine: state.identityQuarantine,
            legacyBlocked: activityLegacyBlocked,
            lastReason: state.lastReason
        };
    }

    return {
        start: start,
        resync: function(reason) { requestResync(reason || 'external_resync', false); },
        forceResync: function(reason) { requestResync(reason || 'external_resync', true); },
        snapshot: snapshot,
        handleBatch: handleBatch,
        handleStatus: handleStatusNotification,
        identityTransition: beginIdentityTransition
    };
}

window.RS.ActivitySequences = {
    isCanonical: activityIsCanonicalU64,
    compare: activityCompareU64,
    increment: activityIncrementU64
};
window.RS.createActivityBootstrap = createActivityBootstrap;

var activityBootstrap = createActivityBootstrap({
    onStatus: function(status) {
        if (activityLegacyBlocked) {
            activityLegacyGenerationFloor = status.ingress_generation;
            activityLegacyBlocked = false;
        }
        applyActivityStatus(status);
    },
    onIdentityTransition: function() {
        _activityControlToken += 1;
        _activityControlPending = false;
        activityStatus = null;
        activityCaptureState = 'off';
        activityEnabled = false;
        activityLevel = 'standard';
        activityLog = [];
        activityStickToBottom = true;
        setActivityControlPending(false);
        updateActivityUI();
        renderActivityFeed();
    },
    onPrivacyBoundary: function(status, reason) {
        _activityControlToken += 1;
        _activityControlPending = false;
        activityStatus = status;
        activityCaptureState = 'off';
        activityEnabled = false;
        activityLevel = 'standard';
        activityLegacyGenerationFloor = status && status.ingress_generation
            ? status.ingress_generation
            : null;
        activityLegacyBlocked = reason === 'identity_hard_reset';
        activityLog = [];
        setActivityControlPending(false);
        updateActivityUI();
        renderActivityFeed();
    },
    onLegacyClear: function(generation) {
        activityPruneLegacyBefore(generation);
        activityStickToBottom = true;
        renderActivityFeed();
    }
});
window.RS.activityBootstrap = activityBootstrap;

function applyActivityStatus(status) {
    if (!activityValidStatus(status)) return false;
    if (activityLegacyBlocked) return false;
    if (activityStatus && activityStatusIsOlder(status, activityStatus)) return false;
    activityStatus = status;
    activityCaptureState = status.state;
    activityEnabled = status.state === 'capturing';
    if (status.state === 'stopped') {
        // Resume is always a Normal capture transition, even when the retained
        // buffer's historical stop boundary reports Trace.
        activityLevel = 'standard';
    } else if (status.profile === 'trace') {
        activityLevel = 'detailed';
    } else if (activityLevel === 'detailed') {
        activityLevel = 'standard';
    }
    updateActivityUI();
    return true;
}

function activityControlElements() {
    var elements = [];
    ['activity-enable-btn', 'activity-enabled-toggle', 'activity-clear-btn'].forEach(function(id) {
        var element = document.getElementById(id);
        if (element) elements.push(element);
    });
    document.querySelectorAll('.activity-level-btn').forEach(function(element) { elements.push(element); });
    return elements;
}

function setActivityControlPending(pending) {
    _activityControlPending = pending;
    activityControlElements().forEach(function(element) {
        element.disabled = pending;
        if (pending) element.setAttribute('aria-busy', 'true');
        else element.removeAttribute('aria-busy');
    });
    if (!pending) updateActivityUI();
}

function beginActivityControl(origin) {
    var context = {
        token: ++_activityControlToken,
        origin: origin,
        focus: document.activeElement,
        enabled: activityEnabled,
        captureState: activityCaptureState,
        status: activityStatus,
        level: activityLevel
    };
    setActivityControlPending(true);
    return context;
}

function finishActivityControl(context) {
    if (context.token !== _activityControlToken) return;
    setActivityControlPending(false);
    var focus = context.focus;
    if (focus && typeof focus.focus === 'function' && (focus.isConnected === undefined || focus.isConnected)) {
        try { focus.focus(); } catch (_) {}
    }
}

function rollbackActivityControl(context) {
    if (context.token !== _activityControlToken) return;
    activityEnabled = context.enabled;
    activityCaptureState = context.captureState;
    activityStatus = context.status;
    activityLevel = context.level;
    updateActivityUI();
}

function reconcileActivityStatus(context) {
    return RS.invoke('activity_status').then(function(status) {
        if (context.token === _activityControlToken && applyActivityStatus(status)) {
            activityBootstrap.forceResync('control_status_reconciled');
        }
        return status;
    }).catch(function() { return null; });
}

function initActivity() {
    updateActivityUI();
    renderActivityFilters();

    var enableBtn = document.getElementById('activity-enable-btn');
    if (enableBtn) {
        enableBtn.addEventListener('click', function() {
            toggleActivityEnabled(true);
        });
    }

    var toggle = document.getElementById('activity-enabled-toggle');
    if (toggle) {
        toggle.addEventListener('change', function() {
            toggleActivityEnabled(this.checked);
        });
    }

    var levelBtns = document.querySelectorAll('.activity-level-btn');
    levelBtns.forEach(function(btn) {
        btn.addEventListener('click', function() {
            setActivityLevel(this.getAttribute('data-level'));
        });
    });

    var clearBtn = document.getElementById('activity-clear-btn');
    if (clearBtn) {
        clearBtn.addEventListener('click', function() {
            clearActivity();
        });
    }

    var feed = document.getElementById('activity-feed');
    if (feed) {
        feed.addEventListener('scroll', function() {
            var distanceFromBottom = feed.scrollHeight - feed.scrollTop - feed.clientHeight;
            activityStickToBottom = distanceFromBottom <= ACTIVITY_STICK_TOLERANCE_PX;
        }, { passive: true });
    }

    RS.listen('network_event', function(data) {
        if (!activityEnabled) return;
        if (!activityLegacyEventAllowed(data)) return;
        addActivityEntry(data);
    });

    RS.listen('network_log_level_changed', function(data) {
        if (!data || !activityValidStatus(data.activity)
            || activityLegacyBlocked
            || (activityStatus && activityStatusIsOlder(data.activity, activityStatus))) {
            return;
        }
        if (data && data.level) {
            activityLevel = data.level;
            updateLevelButtons();
        }
        applyActivityStatus(data.activity);
        if (data && data.restart_required) {
            showToast('Log level updated. Restart required to take effect', 'toast-orange', 5000);
        }
    });

    document.addEventListener('rs-lifecycle-foreground-handled', function(event) {
        var detail = event && event.detail ? event.detail : {};
        if ((typeof isTauriMobile === 'function' && isTauriMobile()) || detail.persisted) {
            activityBootstrap.resync(detail.persisted ? 'pageshow_persisted' : 'mobile_foreground');
        }
    });

    activityBootstrap.start();
}

function toggleActivityEnabled(enabled) {
    var context = beginActivityControl(enabled ? 'start' : 'stop');
    updateActivityUI();
    return RS.invoke('enable_network_log', { args: { enabled: enabled, level: activityLevel } }).then(function(response) {
        if (context.token !== _activityControlToken) return response;
        if (!response || !applyActivityStatus(response.activity)) {
            return reconcileActivityStatus(context).then(function() { return response; });
        }
        activityBootstrap.forceResync('capture_control_acknowledged');
        return response;
    }).then(function(response) {
        if (context.token === _activityControlToken && typeof showToast === 'function') {
            showToast(enabled ? 'Network logging enabled' : 'Network logging stopped', enabled ? 'toast-green' : 'toast-orange', 2000);
        }
        return response;
    }).catch(function(error) {
        rollbackActivityControl(context);
        return reconcileActivityStatus(context).then(function() { throw error; });
    }).catch(function() {
        if (typeof showToast === 'function') showToast('Activity capture could not be changed', 'toast-red', 4000);
    }).then(function(result) {
        finishActivityControl(context);
        return result;
    });
}

function setActivityLevel(level) {
    if (!LEVEL_HIERARCHY.hasOwnProperty(level)) return;
    var context = beginActivityControl('profile');
    activityLevel = level;
    updateLevelButtons();
    return RS.invoke('set_network_log_level', { level: level }).then(function(response) {
        if (context.token !== _activityControlToken) return response;
        if (response && response.level && LEVEL_HIERARCHY.hasOwnProperty(response.level)) {
            activityLevel = response.level;
        }
        if (!response || !applyActivityStatus(response.activity)) {
            return reconcileActivityStatus(context).then(function() { return response; });
        }
        activityBootstrap.forceResync('profile_control_acknowledged');
        return response;
    }).catch(function(error) {
        rollbackActivityControl(context);
        return reconcileActivityStatus(context).then(function() { throw error; });
    }).catch(function() {
        if (typeof showToast === 'function') showToast('Activity profile could not be changed', 'toast-red', 4000);
    }).then(function(result) {
        updateLevelButtons();
        renderActivityFeed();
        finishActivityControl(context);
        return result;
    });
}

function clearActivity() {
    var context = beginActivityControl('clear');
    return RS.invoke('activity_clear').then(function(status) {
        if (context.token !== _activityControlToken) return status;
        if (!applyActivityStatus(status)) throw new Error('Invalid Activity clear acknowledgement');
        activityPruneLegacyBefore(status.ingress_generation);
        activityStickToBottom = true;
        renderActivityFeed();
        activityBootstrap.forceResync('clear_acknowledged');
        return status;
    }).catch(function(error) {
        rollbackActivityControl(context);
        return reconcileActivityStatus(context).then(function() { throw error; });
    }).catch(function() {
        if (typeof showToast === 'function') showToast('Activity could not be cleared', 'toast-red', 4000);
    }).then(function(result) {
        finishActivityControl(context);
        return result;
    });
}

function updateActivityUI() {
    var gate = document.getElementById('activity-privacy-gate');
    var active = document.getElementById('activity-active');
    var clearBtn = document.getElementById('activity-clear-btn');
    var toggle = document.getElementById('activity-enabled-toggle');

    var hasSession = activityCaptureState === 'capturing' || activityCaptureState === 'stopped';
    if (hasSession) {
        if (gate) gate.style.display = 'none';
        if (active) active.style.display = '';
        if (clearBtn) clearBtn.style.display = '';
        if (toggle) toggle.checked = activityCaptureState === 'capturing';
    } else {
        if (gate) gate.style.display = '';
        if (active) active.style.display = 'none';
        if (clearBtn) clearBtn.style.display = 'none';
        if (toggle) toggle.checked = false;
    }
    if (toggle) toggle.disabled = _activityControlPending;
    if (clearBtn) clearBtn.disabled = _activityControlPending;
    var enableBtn = document.getElementById('activity-enable-btn');
    if (enableBtn) enableBtn.disabled = _activityControlPending;
    updateLevelButtons();
}

function updateLevelButtons() {
    var btns = document.querySelectorAll('.activity-level-btn');
    btns.forEach(function(btn) {
        if (btn.getAttribute('data-level') === activityLevel) {
            btn.classList.add('active');
            btn.setAttribute('aria-pressed', 'true');
        } else {
            btn.classList.remove('active');
            btn.setAttribute('aria-pressed', 'false');
        }
        btn.disabled = _activityControlPending || activityCaptureState !== 'capturing';
    });
}

function renderActivityFilters() {
    var container = document.getElementById('activity-filters');
    if (!container) return;

    var html = '';
    var types = ['all', 'announce', 'path', 'message', 'lxst', 'interface', 'link', 'error'];
    for (var i = 0; i < types.length; i++) {
        var type = types[i];
        var label = ACTIVITY_FILTER_LABELS[type];
        var isActive = activityFilters[type];
        html += '<button class="activity-filter-chip' + (isActive ? ' active' : '') + '" data-filter="' + type + '" aria-pressed="' + (isActive ? 'true' : 'false') + '">' + label + '</button>';
    }
    container.innerHTML = html;

    container.querySelectorAll('.activity-filter-chip').forEach(function(chip) {
        chip.addEventListener('click', function() {
            toggleActivityFilter(this.getAttribute('data-filter'));
        });
    });
}

function toggleActivityFilter(type) {
    if (type === 'all') {
        var allOn = activityFilters.all;
        var keys = Object.keys(activityFilters);
        for (var i = 0; i < keys.length; i++) {
            activityFilters[keys[i]] = !allOn;
        }
    } else {
        activityFilters[type] = !activityFilters[type];
        var allSelected = true;
        var filterKeys = ['announce', 'path', 'message', 'lxst', 'interface', 'link', 'error'];
        for (var i = 0; i < filterKeys.length; i++) {
            if (!activityFilters[filterKeys[i]]) { allSelected = false; break; }
        }
        activityFilters.all = allSelected;
    }
    renderActivityFilters();
    renderActivityFeed();
}

function addActivityEntry(entry) {
    if (!activityLegacyEventAllowed(entry)) return;
    var item = {
        type: entry.type || 'interface',
        message: entry.message || '',
        detail: entry.detail || '',
        timestamp: entry.timestamp || Date.now(),
        level: entry.level || 'standard',
        capture_generation: entry.capture_generation
    };

    activityLog.push(item);
    if (activityLog.length > ACTIVITY_MAX_ENTRIES) {
        activityLog = activityLog.slice(-ACTIVITY_MAX_ENTRIES);
    }
    scheduleActivityRender();
}

function scheduleActivityRender() {
    if (_activityRenderScheduled) return;
    _activityRenderScheduled = true;
    requestAnimationFrame(function() {
        _activityRenderScheduled = false;
        renderActivityFeed();
    });
}

function isEntryVisible(entry) {
    if (!activityFilters.all && !activityFilters[entry.type]) return false;
    var entryRank = LEVEL_HIERARCHY[entry.level];
    if (entryRank === undefined) entryRank = 1;
    var configRank = LEVEL_HIERARCHY[activityLevel];
    if (configRank === undefined) configRank = 1;
    if (entryRank > configRank) return false;
    return true;
}

function appendActivityEntry(entry) {
    var feed = document.getElementById('activity-feed');
    if (!feed) return;

    var empty = feed.querySelector('.activity-empty');
    if (empty) empty.remove();

    var div = document.createElement('div');
    div.className = 'activity-entry';
    div.setAttribute('data-type', entry.type);

    var time = formatActivityTime(entry.timestamp);
    div.innerHTML =
        '<span class="activity-entry-time">' + time + '</span>' +
        '<span class="activity-entry-text">' + escapeHtml(entry.message) + '</span>' +
        (entry.detail ? '<span class="activity-entry-detail">' + escapeHtml(entry.detail) + '</span>' : '');

    feed.appendChild(div);
    while (feed.children.length > ACTIVITY_MAX_ENTRIES) {
        feed.removeChild(feed.firstElementChild);
    }

    if (activityStickToBottom) {
        feed.scrollTop = feed.scrollHeight;
    }
}

function renderActivityFeed() {
    var feed = document.getElementById('activity-feed');
    if (!feed) return;

    var filtered = activityLog.filter(isEntryVisible);

    if (filtered.length === 0) {
        feed.innerHTML = '<div class="activity-empty">Listening for network events...</div>';
        return;
    }

    var html = '';
    for (var i = 0; i < filtered.length; i++) {
        var entry = filtered[i];
        var time = formatActivityTime(entry.timestamp);
        html += '<div class="activity-entry" data-type="' + entry.type + '">' +
            '<span class="activity-entry-time">' + time + '</span>' +
            '<span class="activity-entry-text">' + escapeHtml(entry.message) + '</span>' +
            (entry.detail ? '<span class="activity-entry-detail">' + escapeHtml(entry.detail) + '</span>' : '') +
        '</div>';
    }
    feed.innerHTML = html;
    if (activityStickToBottom) {
        feed.scrollTop = feed.scrollHeight;
    }
}

function formatActivityTime(ts) {
    var d = new Date(typeof ts === 'number' ? ts : Date.parse(ts));
    if (isNaN(d.getTime())) return '--:--:--';
    var m = d.getMinutes().toString().padStart(2, '0');
    var s = d.getSeconds().toString().padStart(2, '0');
    if (_use12Hour) {
        var h = d.getHours();
        var period = h >= 12 ? 'PM' : 'AM';
        h = h % 12 || 12;
        return h + ':' + m + ':' + s + ' ' + period;
    }
    return d.getHours().toString().padStart(2, '0') + ':' + m + ':' + s;
}

var REASON_LABELS = {
    Manual: 'manual',
    Malformed: 'malformed',
    RateLimit: 'rate-limited',
    ProtocolViolation: 'protocol violation'
};

function fetchSystemDrops() {
    return RS.invoke('api_network_blackhole')
        .then(function(payload) { return (payload && payload.entries) || []; })
        .catch(function() { return []; });
}

function renderSystemDrops(entries) {
    var card = document.getElementById('system-drops-card');
    var summary = document.getElementById('system-drops-summary');
    var list = document.getElementById('system-drops-list');
    var purgeBtn = document.getElementById('system-drops-purge-unverified-btn');
    if (!card || !summary || !list) return;

    var allEntries = entries || [];
    var systemEntries = allEntries.filter(function(e) { return e.reason !== 'Manual'; });
    // Unverified manual entries \u2014 pre-fix garbage or identities pruned from
    // recent_announces. Count separately so the user has a clear path to clean
    // them up via "Purge unverified".
    var unverifiedManual = allEntries.filter(function(e) {
        return e.reason === 'Manual' && e.verified === false;
    });

    if (purgeBtn) {
        purgeBtn.style.display = unverifiedManual.length > 0 ? '' : 'none';
    }

    if (systemEntries.length === 0 && unverifiedManual.length === 0) {
        card.style.display = 'none';
        return;
    }

    var counts = {};
    systemEntries.forEach(function(e) {
        var label = REASON_LABELS[e.reason] || e.reason || 'unknown';
        counts[label] = (counts[label] || 0) + 1;
    });
    if (unverifiedManual.length > 0) {
        counts['unverified manual'] = unverifiedManual.length;
    }
    var summaryParts = Object.keys(counts).sort().map(function(k) { return counts[k] + ' ' + k; });
    var totalShown = systemEntries.length + unverifiedManual.length;
    summary.textContent = totalShown + ' \u00B7 ' + summaryParts.join(', ');

    var html = '';
    var renderRow = function(e, extraPill) {
        var hashShort = (e.hash || '').substring(0, 16);
        var label = REASON_LABELS[e.reason] || e.reason || 'unknown';
        var pillClass = 'system-drops-pill system-drops-pill-' + (e.reason || 'unknown').toLowerCase();
        var expiry;
        if (typeof e.expires_in === 'number') {
            expiry = e.expires_in > 0 ? formatExpiryShort(Math.floor(e.expires_in)) : 'expired';
        } else {
            expiry = 'no expiry';
        }
        return '<div class="system-drops-row">' +
            '<span class="system-drops-hash" title="' + escapeHtml(e.hash || '') + '">' + escapeHtml(hashShort) + '\u2026</span>' +
            '<span class="' + pillClass + '">' + escapeHtml(label) + '</span>' +
            (extraPill || '') +
            '<span class="system-drops-expiry">' + escapeHtml(expiry) + '</span>' +
        '</div>';
    };
    systemEntries.forEach(function(e) { html += renderRow(e); });
    unverifiedManual.forEach(function(e) {
        html += renderRow(e, '<span class="system-drops-pill system-drops-pill-unverified" title="No announce backs this identity">unverified</span>');
    });
    list.innerHTML = html;
    card.style.display = '';
}

function formatExpiryShort(sec) {
    if (sec >= 86400) return Math.floor(sec / 86400) + 'd';
    if (sec >= 3600) return Math.floor(sec / 3600) + 'h';
    if (sec >= 60) return Math.floor(sec / 60) + 'm';
    return sec + 's';
}

function refreshSystemDrops() {
    fetchSystemDrops().then(renderSystemDrops);
}

function initSystemDrops() {
    var header = document.querySelector('#system-drops-card .system-drops-header');
    var body = document.getElementById('system-drops-body');
    if (header && body) {
        var toggle = function() {
            var open = !body.hasAttribute('hidden');
            if (open) {
                body.setAttribute('hidden', '');
                header.setAttribute('aria-expanded', 'false');
            } else {
                body.removeAttribute('hidden');
                header.setAttribute('aria-expanded', 'true');
            }
        };
        header.addEventListener('click', toggle);
        header.addEventListener('keydown', function(e) {
            if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); toggle(); }
        });
    }

    var clearBtn = document.getElementById('system-drops-clear-btn');
    if (clearBtn) {
        clearBtn.addEventListener('click', function() {
            if (typeof rsConfirm !== 'function') {
                RS.invoke('clear_system_blackholes').catch(function() {});
                return;
            }
            rsConfirm({
                message: 'Clear all system-populated network drops? Manual blocks are not affected.',
                confirmText: 'Clear'
            }).then(function(ok) {
                if (!ok) return;
                RS.invoke('clear_system_blackholes').catch(function() {});
            });
        });
    }

    var purgeBtn = document.getElementById('system-drops-purge-unverified-btn');
    if (purgeBtn) {
        // Drops Manual blackhole entries whose identity is not currently
        // backed by a known announce. Useful after pre-fix builds populated
        // the table with LXMF-dest-hash bytes that can never match an
        // announcer. Also removes legit-but-unseen entries — warn the user.
        purgeBtn.addEventListener('click', function() {
            if (typeof rsConfirm !== 'function') return;
            rsConfirm({
                message: 'Remove network blocks with no recent announce evidence? This cleans up pre-fix garbage entries but may also drop blocks for contacts you have not heard from in a long time.',
                confirmText: 'Purge',
                danger: true
            }).then(function(ok) {
                if (!ok) return;
                RS.invoke('purge_unverified_blackholes').then(function(resp) {
                    if (typeof showToast !== 'function') return;
                    var n = (resp && resp.purged) | 0;
                    if (n === 0) {
                        showToast('Nothing to purge — all blocks are verified.', 'toast-info', 3000);
                    } else {
                        showToast('Purged ' + n + ' unverified entr' + (n === 1 ? 'y' : 'ies') + '.', 'toast-green', 3000);
                    }
                }).catch(function() {});
            });
        });
    }

    RS.listen('blackhole_update', refreshSystemDrops);
    refreshSystemDrops();
}

initSystemDrops();

initActivity();
