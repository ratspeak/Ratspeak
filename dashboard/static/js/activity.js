var activityEvents = [];
var activityCaptureState = 'off';
var activityStatus = null;
var activityProfile = 'normal';
var activityAreaFilter = 'all';
var activityProblemsOnly = false;
var activitySearchQuery = '';
var activityExpandedSequence = null;

var ACTIVITY_MAX_RENDERED = 500;
var _activityRenderScheduled = false;
var _activityControlToken = 0;
var _activityControlPending = false;

// Auto-scroll new entries only when pinned to bottom; 8px tolerance for sub-pixel rounding.
var activityStickToBottom = true;
var ACTIVITY_STICK_TOLERANCE_PX = 8;

var ACTIVITY_AREA_ORDER = [
    'network',
    'interfaces',
    'links',
    'messages',
    'channels',
    'calls',
    'apps',
    'ratspeak'
];

var ACTIVITY_AREA_LABELS = {
    network: 'Network',
    interfaces: 'Interfaces',
    links: 'Links',
    messages: 'Messages',
    channels: 'Channels',
    calls: 'Calls',
    apps: 'Apps',
    ratspeak: 'Ratspeak'
};

var ACTIVITY_SUBJECT_LABELS = {
    'diagnostics.capture': 'Capture',
    'diagnostics.worker': 'Activity recorder',
    'diagnostics': 'Activity',
    'app.runtime': 'Ratspeak',
    'storage.db': 'Local storage',
    'ipc': 'App event',
    'rns.transport': 'Reticulum',
    'rns.path': 'Path',
    'rns.announce': 'Announce',
    'rns.security': 'Network input',
    'rns.packet': 'Packet',
    'rns.link': 'Link',
    'resource': 'Resource transfer',
    'lxmf.delivery': 'Message',
    'lxmf.propagation': 'Offline Inbox',
    'lxmf.inbound': 'Incoming message',
    'lxst.service': 'Voice service',
    'lxst.call': 'Call',
    'lxst.media': 'Call media',
    'channels.session': 'Channel session',
    'channels.room': 'Channel',
    'channels.envelope': 'Channel message',
    'channels.heartbeat': 'Channel heartbeat',
    'interface': 'Interface',
    'lrgp.action': 'Game action'
};
var ACTIVITY_SUBJECT_PREFIXES = Object.keys(ACTIVITY_SUBJECT_LABELS).sort(function(left, right) {
    return right.length - left.length;
});

var ACTIVITY_U64_MAX = '18446744073709551615';
var ACTIVITY_REPLAY_MAX_EVENTS = 50;
var ACTIVITY_REPLAY_MAX_BYTES = 65536;
var ACTIVITY_LISTENER_RETRY_DELAYS = [100, 200, 400, 800, 1600, 2000];

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
    var onEvents = deps.onEvents || function() {};
    var onIdentityTransition = deps.onIdentityTransition || function() {};
    var onPrivacyBoundary = deps.onPrivacyBoundary || function() {};
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

    function publishEvents() {
        onEvents(state.events.slice(), state.status, state.phase);
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
        publishEvents();
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
            publishEvents();
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
        publishEvents();
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
        applyActivityStatus(status);
    },
    onEvents: function(events) {
        activityEvents = events.slice();
        if (activityExpandedSequence && !activityEvents.some(function(event) {
            return event.sequence === activityExpandedSequence;
        })) {
            activityExpandedSequence = null;
        }
        scheduleActivityRender();
    },
    onIdentityTransition: function() {
        _activityControlToken += 1;
        _activityControlPending = false;
        activityStatus = null;
        activityCaptureState = 'off';
        activityProfile = 'normal';
        activityEvents = [];
        activityAreaFilter = 'all';
        activityProblemsOnly = false;
        activitySearchQuery = '';
        activityExpandedSequence = null;
        activityStickToBottom = true;
        setActivityControlPending(false);
        updateActivityUI();
        renderActivityFilters();
        renderActivityFeed();
    },
    onPrivacyBoundary: function(status) {
        _activityControlToken += 1;
        _activityControlPending = false;
        activityStatus = status;
        activityCaptureState = 'off';
        activityProfile = 'normal';
        activityEvents = [];
        activityExpandedSequence = null;
        setActivityControlPending(false);
        updateActivityUI();
        renderActivityFilters();
        renderActivityFeed();
    }
});
window.RS.activityBootstrap = activityBootstrap;

function applyActivityStatus(status) {
    if (!activityValidStatus(status)) return false;
    if (activityStatus && activityStatusIsOlder(status, activityStatus)) return false;
    activityStatus = status;
    activityCaptureState = status.state;
    activityProfile = status.profile === 'trace' ? 'trace' : 'normal';
    updateActivityUI();
    return true;
}

function activityControlElements() {
    var elements = [];
    ['activity-enable-btn', 'activity-capture-btn', 'activity-clear-btn'].forEach(function(id) {
        var element = document.getElementById(id);
        if (element) elements.push(element);
    });
    document.querySelectorAll('.activity-profile-btn').forEach(function(element) { elements.push(element); });
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
        captureState: activityCaptureState,
        status: activityStatus,
        profile: activityProfile
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
    activityCaptureState = context.captureState;
    activityStatus = context.status;
    activityProfile = context.profile;
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
    renderActivityFeed();

    var enableBtn = document.getElementById('activity-enable-btn');
    if (enableBtn) {
        enableBtn.addEventListener('click', function() {
            setActivityCapture(true);
        });
    }

    var captureBtn = document.getElementById('activity-capture-btn');
    if (captureBtn) {
        captureBtn.addEventListener('click', function() {
            setActivityCapture(activityCaptureState !== 'capturing');
        });
    }

    document.querySelectorAll('.activity-profile-btn').forEach(function(btn) {
        btn.addEventListener('click', function() {
            setActivityProfile(this.getAttribute('data-profile'));
        });
    });

    var clearBtn = document.getElementById('activity-clear-btn');
    if (clearBtn) {
        clearBtn.addEventListener('click', function() {
            clearActivity();
        });
    }

    var searchInput = document.getElementById('activity-search-input');
    if (searchInput) {
        searchInput.addEventListener('input', function() {
            activitySearchQuery = this.value.trim().toLowerCase();
            activityStickToBottom = !activitySearchQuery;
            renderActivityFeed();
        });
    }

    var filters = document.getElementById('activity-filters');
    if (filters) {
        filters.addEventListener('click', function(event) {
            var button = activityClosestEventTarget(event.target, 'activity-filter-chip', filters);
            if (!button) return;
            var area = button.getAttribute('data-area');
            if (area) {
                activityAreaFilter = area;
            } else if (button.getAttribute('data-problems') === 'true') {
                activityProblemsOnly = !activityProblemsOnly;
            }
            activityStickToBottom = false;
            renderActivityFilters();
            renderActivityFeed();
        });
    }

    var feed = document.getElementById('activity-feed');
    if (feed) {
        feed.addEventListener('scroll', function() {
            var distanceFromBottom = feed.scrollHeight - feed.scrollTop - feed.clientHeight;
            activityStickToBottom = distanceFromBottom <= ACTIVITY_STICK_TOLERANCE_PX;
        }, { passive: true });
        feed.addEventListener('click', function(event) {
            var row = activityClosestEventTarget(event.target, 'activity-event', feed);
            if (row) toggleActivityEvent(row.getAttribute('data-sequence'));
        });
        feed.addEventListener('keydown', function(event) {
            if (event.key !== 'Enter' && event.key !== ' ') return;
            var row = activityClosestEventTarget(event.target, 'activity-event', feed);
            if (!row) return;
            event.preventDefault();
            toggleActivityEvent(row.getAttribute('data-sequence'));
        });
    }

    document.addEventListener('rs-lifecycle-foreground-handled', function(event) {
        var detail = event && event.detail ? event.detail : {};
        if ((typeof isTauriMobile === 'function' && isTauriMobile()) || detail.persisted) {
            activityBootstrap.resync(detail.persisted ? 'pageshow_persisted' : 'mobile_foreground');
        }
    });

    activityBootstrap.start();
}

function setActivityCapture(enabled) {
    var command = null;
    var toastText = null;
    var toastClass = 'toast-green';
    if (enabled && activityCaptureState === 'off') {
        command = 'activity_start';
        toastText = 'Activity started';
    } else if (enabled && activityCaptureState === 'stopped') {
        command = 'activity_resume';
        toastText = 'Activity resumed';
    } else if (!enabled && activityCaptureState === 'capturing') {
        command = 'activity_stop';
        toastText = 'Activity paused';
        toastClass = 'toast-orange';
    }
    if (!command || _activityControlPending) return Promise.resolve(activityStatus);

    var context = beginActivityControl(command);
    updateActivityUI();
    return RS.invoke(command).then(function(status) {
        if (context.token !== _activityControlToken) return status;
        if (!applyActivityStatus(status)) {
            return reconcileActivityStatus(context).then(function() { return status; });
        }
        activityBootstrap.forceResync('capture_control_acknowledged');
        return status;
    }).then(function(status) {
        if (context.token === _activityControlToken && typeof showToast === 'function') {
            showToast(toastText, toastClass, 2000);
        }
        return status;
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

function setActivityProfile(profile) {
    if (profile !== 'normal' && profile !== 'trace') return Promise.resolve(activityStatus);
    if (activityCaptureState !== 'capturing' || profile === activityProfile || _activityControlPending) {
        return Promise.resolve(activityStatus);
    }
    var context = beginActivityControl('profile');
    return RS.invoke('activity_set_profile', { args: { profile: profile } }).then(function(status) {
        if (context.token !== _activityControlToken) return status;
        if (!applyActivityStatus(status)) {
            return reconcileActivityStatus(context).then(function() { return status; });
        }
        activityBootstrap.forceResync('profile_control_acknowledged');
        return status;
    }).catch(function(error) {
        rollbackActivityControl(context);
        return reconcileActivityStatus(context).then(function() { throw error; });
    }).catch(function() {
        if (typeof showToast === 'function') showToast('Activity profile could not be changed', 'toast-red', 4000);
    }).then(function(result) {
        updateProfileButtons();
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
        activityEvents = [];
        activityExpandedSequence = null;
        activityStickToBottom = true;
        renderActivityFilters();
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
    var captureBtn = document.getElementById('activity-capture-btn');
    var status = document.getElementById('activity-status');
    var statusLabel = document.getElementById('activity-status-label');

    var hasSession = activityCaptureState === 'capturing' || activityCaptureState === 'stopped';
    if (hasSession) {
        if (gate) gate.style.display = 'none';
        if (active) active.style.display = '';
        if (clearBtn) clearBtn.style.display = '';
        if (captureBtn) {
            captureBtn.style.display = '';
            captureBtn.textContent = activityCaptureState === 'capturing' ? 'Pause' : 'Resume';
            captureBtn.setAttribute(
                'aria-label',
                activityCaptureState === 'capturing' ? 'Pause Activity capture' : 'Resume Activity capture'
            );
            captureBtn.setAttribute('data-state', activityCaptureState);
        }
    } else {
        if (gate) gate.style.display = '';
        if (active) active.style.display = 'none';
        if (clearBtn) clearBtn.style.display = 'none';
        if (captureBtn) captureBtn.style.display = 'none';
    }
    if (status) status.setAttribute('data-state', activityCaptureState);
    if (statusLabel) {
        statusLabel.textContent = activityCaptureState === 'capturing'
            ? 'Recording'
            : (activityCaptureState === 'stopped' ? 'Paused' : 'Off');
    }
    if (captureBtn) captureBtn.disabled = _activityControlPending;
    if (clearBtn) clearBtn.disabled = _activityControlPending || activityEvents.length === 0;
    var enableBtn = document.getElementById('activity-enable-btn');
    if (enableBtn) enableBtn.disabled = _activityControlPending;
    updateProfileButtons();
}

function updateProfileButtons() {
    var btns = document.querySelectorAll('.activity-profile-btn');
    btns.forEach(function(btn) {
        if (btn.getAttribute('data-profile') === activityProfile) {
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

    var areaCounts = Object.create(null);
    var problemCount = 0;
    for (var i = 0; i < activityEvents.length; i++) {
        var event = activityEvents[i];
        areaCounts[event.area] = (areaCounts[event.area] || 0) + 1;
        if (activityIsProblem(event)) problemCount++;
    }
    if (activityAreaFilter !== 'all' && !areaCounts[activityAreaFilter]) {
        activityAreaFilter = 'all';
    }
    var html = '<button class="activity-filter-chip' + (activityAreaFilter === 'all' ? ' active' : '') +
        '" data-area="all" aria-pressed="' + (activityAreaFilter === 'all' ? 'true' : 'false') + '">All</button>';
    for (var areaIndex = 0; areaIndex < ACTIVITY_AREA_ORDER.length; areaIndex++) {
        var area = ACTIVITY_AREA_ORDER[areaIndex];
        if (!areaCounts[area]) continue;
        var isActive = activityAreaFilter === area;
        html += '<button class="activity-filter-chip' + (isActive ? ' active' : '') +
            '" data-area="' + area + '" aria-pressed="' + (isActive ? 'true' : 'false') + '">' +
            escapeHtml(ACTIVITY_AREA_LABELS[area] || activityHumanizeCode(area)) + '</button>';
    }
    if (problemCount > 0) {
        html += '<button class="activity-filter-chip activity-filter-problems' +
            (activityProblemsOnly ? ' active' : '') +
            '" data-problems="true" aria-pressed="' + (activityProblemsOnly ? 'true' : 'false') +
            '">Problems <span>' + problemCount + '</span></button>';
    } else {
        activityProblemsOnly = false;
    }
    container.innerHTML = html;
}

function activityClosestEventTarget(target, className, boundary) {
    var current = target;
    while (current && current !== boundary) {
        if (current.classList && current.classList.contains(className)) return current;
        current = current.parentElement;
    }
    return null;
}

function toggleActivityEvent(sequence) {
    if (!activityIsCanonicalU64(sequence, false)) return;
    activityExpandedSequence = activityExpandedSequence === sequence ? null : sequence;
    activityStickToBottom = false;
    renderActivityFeed();
    var row = document.querySelector('[data-activity-sequence="' + sequence + '"]');
    if (row && typeof row.focus === 'function') {
        try { row.focus({ preventScroll: true }); } catch (_) { row.focus(); }
    }
}

function scheduleActivityRender() {
    if (_activityRenderScheduled) return;
    _activityRenderScheduled = true;
    requestAnimationFrame(function() {
        _activityRenderScheduled = false;
        renderActivityFilters();
        renderActivityFeed();
        updateActivityUI();
    });
}

function activityIsProblem(event) {
    if (event.severity === 'warning' || event.severity === 'error') return true;
    return ['degraded', 'rejected', 'failed', 'timed_out', 'dropped'].indexOf(event.outcome) !== -1;
}

function activityEventMatches(event) {
    if (activityAreaFilter !== 'all' && event.area !== activityAreaFilter) return false;
    if (activityProblemsOnly && !activityIsProblem(event)) return false;
    if (!activitySearchQuery) return true;
    return activityEventSearchText(event).indexOf(activitySearchQuery) !== -1;
}

function activityEventSearchText(event) {
    var values = [
        activityEventSummary(event),
        event.kind,
        event.summary_code,
        event.area,
        event.direction,
        event.outcome,
        event.severity,
        event.correlation_id || ''
    ];
    (event.attributes || []).forEach(function(attribute) {
        values.push(attribute.key, activityFormatAttributeValue(attribute));
    });
    return values.join(' ').toLowerCase();
}

function activityAttribute(event, key) {
    var attributes = event && Array.isArray(event.attributes) ? event.attributes : [];
    for (var i = 0; i < attributes.length; i++) {
        if (attributes[i].key === key) return attributes[i];
    }
    return null;
}

function activityCodeValue(event, key) {
    var attribute = activityAttribute(event, key);
    return attribute && attribute.value && attribute.value.type === 'code'
        ? attribute.value.value
        : null;
}

function activityEventSummary(event) {
    var code = event.summary_code || event.kind || 'activity.event';
    var special = {
        'diagnostics.dropped': 'Activity events dropped',
        'diagnostics.evicted': 'Older Activity events removed',
        'diagnostics.worker_recovered': 'Activity recorder recovered',
        'storage.db.failed': 'Local storage unavailable',
        'ipc.failed': 'App event delivery failed',
        'rns.security.dropped': 'Network input rejected',
        'rns.announce.ingress_burst_started': 'High announce traffic detected',
        'rns.announce.ingress_burst_cleared': 'Announce traffic returned to normal',
        'lxmf.propagation.started': 'Storing message in Offline Inbox',
        'lxmf.propagation.succeeded': 'Message stored in Offline Inbox',
        'lxmf.propagation.failed': 'Offline Inbox delivery failed',
        'channels.session.greeting_observed': 'Hub greeting received',
        'channels.session.negotiated': 'Channel session ready'
    };
    if (special[code]) return special[code];

    var prefix = '';
    var subject = '';
    for (var i = 0; i < ACTIVITY_SUBJECT_PREFIXES.length; i++) {
        var candidate = ACTIVITY_SUBJECT_PREFIXES[i];
        if (code === candidate || code.indexOf(candidate + '.') === 0) {
            prefix = candidate;
            subject = ACTIVITY_SUBJECT_LABELS[candidate];
            break;
        }
    }
    if (prefix === 'interface') {
        subject = activityInterfaceLabel(activityCodeValue(event, 'interface_class'));
    }
    if (!subject) {
        var pieces = code.split('.');
        subject = activityHumanizeCode(pieces.slice(0, Math.max(1, pieces.length - 1)).join(' '));
        prefix = pieces.slice(0, Math.max(1, pieces.length - 1)).join('.');
    }
    var actionCode = code === prefix ? '' : code.slice(prefix.length + 1);
    var actions = {
        'ready': 'ready',
        'started': 'started',
        'stopped': 'stopped',
        'unavailable': 'unavailable',
        'failed': 'failed',
        'requested': 'requested',
        'discovered': 'found',
        'observed': 'observed',
        'timed_out': 'timed out',
        'sent': 'sent',
        'held': 'queued',
        'suppressed': 'suppressed',
        'dropped': 'dropped',
        'authenticated': 'authenticated',
        'identified': 'identified',
        'stale': 'became stale',
        'recovered': 'recovered',
        'closed': 'closed',
        'progress': 'in progress',
        'succeeded': 'completed',
        'queued': 'queued',
        'submission_failed': 'could not be queued',
        'method_selected': 'delivery method selected',
        'path_pending': 'waiting for a path',
        'link_establishing': 'establishing a link',
        'link_ready': 'link ready',
        'link_reused': 'link reused',
        'direct_pending': 'waiting for direct delivery',
        'resource_started': 'resource transfer started',
        'awaiting_proof': 'awaiting proof',
        'delivered': 'delivered',
        'rejected': 'rejected',
        'deferred': 'deferred',
        'retrying': 'retrying',
        'accepted': 'received',
        'connecting': 'connecting',
        'configured': 'configured',
        'cancelled': 'cancelled',
        'online': 'online',
        'offline': 'offline',
        'degraded': 'degraded',
        'paused': 'paused',
        'removed': 'removed',
        'connect_requested': 'connection requested',
        'path_requested': 'path requested',
        'path_discovered': 'path found',
        'path_timed_out': 'path timed out',
        'link_requested': 'link requested',
        'link_authenticated': 'link authenticated',
        'link_identification_sent': 'identity sent',
        'hello_sent': 'hello sent',
        'welcome_validated': 'welcome accepted',
        'welcome_rejected': 'welcome rejected',
        'join_requested': 'join requested',
        'join_cancelled': 'join cancelled',
        'join_rejected': 'join rejected',
        'join_timed_out': 'join timed out',
        'joined': 'joined',
        'part_requested': 'leave requested',
        'part_cancelled': 'leave cancelled',
        'part_rejected': 'leave rejected',
        'part_timed_out': 'leave timed out',
        'parted': 'left',
        'received': 'received',
        'ping': 'ping sent',
        'pong': 'pong received',
        'profile_changed': 'detail changed',
        'capture_started': 'started',
        'capture_stopped': 'paused',
        'capture_resumed': 'resumed',
        'capture_cleared': 'cleared'
    };
    return (subject + (actionCode ? ' ' + (actions[actionCode] || activityHumanizeCode(actionCode).toLowerCase()) : '')).trim();
}

function activityInterfaceLabel(code) {
    var labels = {
        auto: 'Local Network',
        ble_peer: 'Bluetooth Peer',
        rnode: 'LoRa radio',
        tcp_client: 'TCP client',
        tcp_server: 'TCP server',
        backbone_client: 'Backbone client',
        backbone_server: 'Backbone server'
    };
    return labels[code] || 'Interface';
}

function activityHumanizeCode(value) {
    var text = String(value || '').replace(/[._]+/g, ' ').trim();
    return text ? text.charAt(0).toUpperCase() + text.slice(1) : 'Unknown';
}

function activityFormatBytes(bytes) {
    var value = Number(bytes);
    if (!isFinite(value) || value < 0) return String(bytes);
    if (value < 1024) return value + ' B';
    var units = ['KB', 'MB', 'GB'];
    var unit = -1;
    do {
        value /= 1024;
        unit++;
    } while (value >= 1024 && unit < units.length - 1);
    return (value >= 10 ? value.toFixed(0) : value.toFixed(1)) + ' ' + units[unit];
}

function activityShortToken(value) {
    var text = String(value || '');
    if (text.length <= 16) return text;
    return text.slice(0, 9) + '…' + text.slice(-4);
}

function activityFormatAttributeValue(attribute) {
    if (!attribute || !attribute.value) return 'Unknown';
    var type = attribute.value.type;
    var value = attribute.value.value;
    if (type === 'boolean') return value ? 'Yes' : 'No';
    if (type === 'code') return String(value);
    if (type === 'endpoint') {
        return activityHumanizeCode(value && value.class ? value.class : 'unknown') + ' endpoint';
    }
    if (type === 'identifier') {
        var kind = activityHumanizeCode(value && value.kind ? value.kind : attribute.key);
        var token = value && value.pseudonym ? activityShortToken(value.pseudonym) : '';
        if (value && value.ordinal != null) {
            return kind + ' ' + value.ordinal + (token ? ' · ' + token : '');
        }
        return token ? kind + ' · ' + token : kind;
    }
    if (attribute.key === 'byte_length' || attribute.key === 'mdu' || attribute.key === 'max_message_bytes'
        || attribute.key === 'max_nick_bytes' || attribute.key === 'max_room_bytes') {
        return activityFormatBytes(value);
    }
    if (attribute.key === 'duration_ms' || attribute.key === 'rtt_ms' || attribute.key === 'time_span_ms') {
        return String(value) + ' ms';
    }
    if (attribute.key === 'percent') return String(value) + '%';
    return String(value);
}

function activityAttributeLabel(key) {
    var labels = {
        byte_length: 'Size',
        dropped_count: 'Dropped',
        duplicate: 'Duplicate',
        duration_ms: 'Duration',
        endpoint: 'Endpoint',
        evicted_count: 'Removed',
        interface_class: 'Interface',
        max_message_bytes: 'Maximum message',
        max_nick_bytes: 'Maximum nickname',
        max_room_bytes: 'Maximum channel name',
        max_rooms: 'Maximum channels',
        queue_count: 'Queued',
        rate_per_minute: 'Rate',
        rtt_ms: 'Round trip',
        time_span_ms: 'Time span'
    };
    return labels[key] || activityHumanizeCode(key);
}

function activityOutcomeLabel(outcome) {
    var labels = {
        timed_out: 'Timed out',
        in_progress: 'In progress'
    };
    return labels[outcome] || activityHumanizeCode(outcome);
}

function activityTimestampIso(timestamp) {
    var date = new Date(timestamp);
    return isNaN(date.getTime()) ? '' : date.toISOString();
}

function activityEventDetails(event) {
    var rows = [
        ['Event', '<code>' + escapeHtml(event.kind) + '</code>']
    ];
    (event.attributes || []).forEach(function(attribute) {
        rows.push([
            activityAttributeLabel(attribute.key),
            '<code>' + escapeHtml(activityFormatAttributeValue(attribute)) + '</code>'
        ]);
    });
    if (event.correlation_id) {
        rows.push(['Correlation', '<code>' + escapeHtml(activityShortToken(event.correlation_id)) + '</code>']);
    }
    if (event.direction && event.direction !== 'none') {
        rows.push(['Direction', escapeHtml(activityHumanizeCode(event.direction))]);
    }
    rows.push(['Capture', escapeHtml(activityHumanizeCode(event.capture_profile || 'normal'))]);
    rows.push(['Sequence', '<code>#' + escapeHtml(event.sequence) + '</code>']);

    var html = '<dl class="activity-event-details">';
    for (var i = 0; i < rows.length; i++) {
        html += '<div><dt>' + escapeHtml(rows[i][0]) + '</dt><dd>' + rows[i][1] + '</dd></div>';
    }
    return html + '</dl>';
}

function renderActivityFeed() {
    var feed = document.getElementById('activity-feed');
    if (!feed) return;

    var filtered = activityEvents.filter(activityEventMatches);
    var visible = filtered.length > ACTIVITY_MAX_RENDERED
        ? filtered.slice(filtered.length - ACTIVITY_MAX_RENDERED)
        : filtered;
    var count = document.getElementById('activity-result-count');
    if (count) {
        if (filtered.length === activityEvents.length) {
            count.textContent = filtered.length + (filtered.length === 1 ? ' event' : ' events');
        } else {
            count.textContent = filtered.length + ' of ' + activityEvents.length;
        }
        if (visible.length < filtered.length) {
            count.textContent = 'Latest ' + visible.length + ' of ' + filtered.length;
        }
    }

    if (visible.length === 0) {
        var emptyMessage = activityEvents.length
            ? 'No activity matches these filters.'
            : (activityCaptureState === 'stopped' ? 'No activity was captured.' : 'Waiting for activity…');
        feed.innerHTML = '<div class="activity-empty">' + emptyMessage + '</div>';
        return;
    }

    var html = '';
    for (var i = 0; i < visible.length; i++) {
        var event = visible[i];
        var expanded = event.sequence === activityExpandedSequence;
        var problem = activityIsProblem(event);
        var outcome = event.outcome && event.outcome !== 'none' ? event.outcome : '';
        html += '<div class="activity-event' + (expanded ? ' expanded' : '') + (problem ? ' is-problem' : '') +
            '" data-area="' + escapeHtml(event.area) + '" data-sequence="' + escapeHtml(event.sequence) +
            '" data-activity-sequence="' + escapeHtml(event.sequence) + '" role="button" tabindex="0" aria-expanded="' +
            (expanded ? 'true' : 'false') + '">' +
                '<span class="activity-event-rail"><span></span></span>' +
                '<div class="activity-event-content">' +
                    '<div class="activity-event-heading">' +
                        '<span class="activity-event-summary">' + escapeHtml(activityEventSummary(event)) + '</span>' +
                        '<time class="activity-event-time" datetime="' +
                            escapeHtml(activityTimestampIso(event.timestamp_unix_ms)) + '">' +
                            escapeHtml(formatActivityTime(event.timestamp_unix_ms)) + '</time>' +
                    '</div>' +
                    '<div class="activity-event-meta">' +
                        '<span>' + escapeHtml(ACTIVITY_AREA_LABELS[event.area] || activityHumanizeCode(event.area)) + '</span>' +
                        (outcome ? '<span data-outcome="' + escapeHtml(outcome) + '">' +
                            escapeHtml(activityOutcomeLabel(outcome)) + '</span>' : '') +
                        (event.count > 1 ? '<span>' + escapeHtml(String(event.count)) + ' events</span>' : '') +
                    '</div>' +
                    (expanded ? activityEventDetails(event) : '') +
                '</div>' +
                '<svg class="activity-event-chevron" viewBox="0 0 24 24" width="15" height="15" aria-hidden="true">' +
                    '<polyline points="6 9 12 15 18 9"></polyline>' +
                '</svg>' +
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
