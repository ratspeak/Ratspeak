#!/usr/bin/env node
// Deterministic behavioral tests for Stage 1C's listener-first Activity bootstrap.
// Plain Node, no browser or network dependencies.

'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var activityPath = path.join(__dirname, '..', 'static', 'js', 'activity.js');
var statePath = path.join(__dirname, '..', 'static', 'js', 'state.js');
var indexPath = path.join(__dirname, '..', 'index.html');
var activitySource = fs.readFileSync(activityPath, 'utf8');
var stateSource = fs.readFileSync(statePath, 'utf8');
var indexSource = fs.readFileSync(indexPath, 'utf8');

var SESSION_A = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa';
var SESSION_B = 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb';
var tests = [];

function test(name, fn) {
    tests.push({ name: name, fn: fn });
}

function deferred() {
    var resolve;
    var reject;
    var promise = new Promise(function(onResolve, onReject) {
        resolve = onResolve;
        reject = onReject;
    });
    return { promise: promise, resolve: resolve, reject: reject };
}

async function flush(turns) {
    var count = turns == null ? 24 : turns;
    for (var i = 0; i < count; i++) await Promise.resolve();
}

function status(session, state, oldest, latest, profile, generation, workerEpoch) {
    return {
        version: 1,
        capture_session: session,
        state: state,
        profile: profile == null ? (session ? 'normal' : null) : profile,
        ingress_generation: generation || '1',
        oldest: oldest,
        latest: latest,
        worker_state: 'running',
        worker_epoch: workerEpoch || '1',
        counters: {}
    };
}

function event(session, sequence, marker, generation) {
    return {
        version: 1,
        sequence: sequence,
        capture_session: session,
        capture_generation: generation || '1',
        timestamp_unix_ms: 1,
        elapsed_ms: 1,
        area: 'ratspeak',
        kind: marker || ('test.' + sequence),
        severity: 'info',
        capture_profile: 'normal',
        direction: 'none',
        outcome: 'none',
        summary_code: 'test',
        attributes: [],
        count: 1
    };
}

function batch(session, events) {
    var ordered = events.slice().sort(function(left, right) {
        if (left.sequence.length !== right.sequence.length) return left.sequence.length - right.sequence.length;
        return left.sequence < right.sequence ? -1 : (left.sequence > right.sequence ? 1 : 0);
    });
    return {
        version: 1,
        capture_session: session,
        first_sequence: ordered[0].sequence,
        last_sequence: ordered[ordered.length - 1].sequence,
        events: events
    };
}

function replayPage(session, events, opts) {
    opts = opts || {};
    return {
        result: 'page',
        page: {
            version: 1,
            capture_session: session,
            events: events,
            oldest: opts.oldest == null ? (events.length ? events[0].sequence : null) : opts.oldest,
            latest: opts.latest == null ? (events.length ? events[events.length - 1].sequence : null) : opts.latest,
            next_after: opts.nextAfter == null ? (events.length ? events[events.length - 1].sequence : null) : opts.nextAfter,
            has_more: !!opts.hasMore,
            gap: !!opts.gap,
            status_counters: {}
        }
    };
}

function loadControllerLibrary() {
    var start = activitySource.indexOf('var ACTIVITY_U64_MAX');
    var end = activitySource.indexOf('\nvar activityBootstrap =', start);
    assert(start !== -1 && end !== -1, 'controller source markers must exist');
    var window = { RS: { diag: function() {} } };
    var context = {
        window: window,
        RS: window.RS,
        Promise: Promise,
        JSON: JSON,
        Object: Object,
        Array: Array,
        String: String,
        setTimeout: setTimeout,
        clearTimeout: clearTimeout,
        isMobile: function() { return false; }
    };
    vm.runInNewContext(activitySource.slice(start, end), context, { filename: 'activity-controller.js' });
    return window.RS;
}

var controllerLibrary = loadControllerLibrary();

function loadPresentationLibrary() {
    var start = activitySource.indexOf('function activityAttribute(event, key)');
    var end = activitySource.indexOf('\nfunction activityTimestampIso', start);
    assert(start !== -1 && end !== -1, 'presentation source markers must exist');
    var window = {};
    var context = {
        window: window,
        ACTIVITY_AREA_LABELS: {
            network: 'Network',
            channels: 'Channels',
            ratspeak: 'Ratspeak'
        },
        Array: Array,
        Object: Object,
        String: String
    };
    var exportSource = '\nwindow.ActivityPresentation = {' +
        'summary: activityEventSummary,' +
        'format: activityFormatAttributeValue,' +
        'label: activityAttributeLabel,' +
        'area: activityPresentationArea' +
        '};';
    vm.runInNewContext(activitySource.slice(start, end) + exportSource, context, {
        filename: 'activity-presentation.js'
    });
    return window.ActivityPresentation;
}

var presentationLibrary = loadPresentationLibrary();

function makeImmediateController(invoke, opts) {
    opts = opts || {};
    var handlers = {};
    var listenOrder = [];
    var invokeCalls = [];
    var statuses = [];
    var eventSnapshots = [];
    var identityTransitions = 0;
    var controller = controllerLibrary.createActivityBootstrap({
        listen: function(name, handler, options) {
            listenOrder.push({ name: name, options: options });
            handlers[name] = handler;
            return Promise.resolve(function() {});
        },
        invoke: function(name, args) {
            invokeCalls.push({ name: name, args: args });
            return invoke(name, args, invokeCalls.length);
        },
        maxEvents: opts.maxEvents || 5000,
        retryDelays: opts.retryDelays || [],
        setTimeout: opts.setTimeout,
        clearTimeout: opts.clearTimeout,
        onStatus: function(value) {
            statuses.push(value);
            if (typeof opts.onStatus === 'function') opts.onStatus(value);
        },
        onEvents: function(values) {
            eventSnapshots.push(values);
            if (typeof opts.onEvents === 'function') opts.onEvents(values);
        },
        onIdentityTransition: function() { identityTransitions++; },
        diagnose: function() {}
    });
    return {
        controller: controller,
        handlers: handlers,
        listenOrder: listenOrder,
        invokeCalls: invokeCalls,
        statuses: statuses,
        eventSnapshots: eventSnapshots,
        identityTransitions: function() { return identityTransitions; }
    };
}

test('RS.listen required mode rejects a static bridge error and legacy mode remains compatible', async function() {
    var start = stateSource.indexOf('window.RS.listen = function');
    var end = stateSource.indexOf('\n};\n\n// Fetch an LXMF file attachment', start);
    assert(start !== -1 && end !== -1, 'RS.listen source markers must exist');
    var intervals = [];
    var window = { RS: { diag: function() {} } };
    var context = {
        window: window,
        Promise: Promise,
        setInterval: function(callback) { intervals.push(callback); return intervals.length; },
        clearInterval: function() {}
    };
    vm.runInNewContext(stateSource.slice(start, end + 3), context, { filename: 'state-listen.js' });

    var required = window.RS.listen('required', function() {}, { required: true });
    for (var i = 0; i < 20; i++) intervals[0]();
    await assert.rejects(required, function(error) {
        return error && error.code === 'event_bridge_unavailable'
            && error.message === 'Required Tauri event bridge is unavailable';
    });

    var legacy = window.RS.listen('legacy', function() {});
    for (var j = 0; j < 20; j++) intervals[1]();
    assert.strictEqual(typeof await legacy, 'function');
});

test('all identity and batch listeners attach before the first status query', async function() {
    var gates = [];
    var listenOrder = [];
    var calls = [];
    var controller = controllerLibrary.createActivityBootstrap({
        listen: function(name, handler, options) {
            var gate = deferred();
            gates.push(gate);
            listenOrder.push({ name: name, required: options && options.required });
            return gate.promise;
        },
        invoke: function(name) {
            calls.push(name);
            return Promise.resolve(status(null, 'off', null, null, null));
        },
        retryDelays: [],
        diagnose: function() {}
    });

    controller.start();
    await flush();
    assert.deepStrictEqual(calls, []);
    var expected = [
        'identity_switching',
        'identity_switched',
        'identity_error',
        'activity_status_v1',
        'activity_boundary_v1',
        'activity_batch_v1'
    ];
    for (var i = 0; i < expected.length; i++) {
        assert.strictEqual(listenOrder[i].name, expected[i]);
        assert.strictEqual(listenOrder[i].required, true);
        assert.deepStrictEqual(calls, []);
        gates[i].resolve(function() {});
        await flush();
    }
    assert.strictEqual(calls[0], 'activity_status');
    await flush();
    assert.strictEqual(controller.snapshot().phase, 'live');
});

test('required listener failure retries with bounded scheduling before status', async function() {
    var scheduled = [];
    var attempts = 0;
    var calls = [];
    var controller = controllerLibrary.createActivityBootstrap({
        listen: function() {
            attempts++;
            if (attempts === 1) {
                var error = new Error('static');
                error.code = 'event_bridge_unavailable';
                return Promise.reject(error);
            }
            return Promise.resolve(function() {});
        },
        invoke: function(name) {
            calls.push(name);
            return Promise.resolve(status(null, 'off', null, null, null));
        },
        retryDelays: [7],
        setTimeout: function(callback, delay) { scheduled.push({ callback: callback, delay: delay }); return 1; },
        clearTimeout: function() {},
        diagnose: function() {}
    });
    controller.start();
    await flush();
    assert.strictEqual(scheduled.length, 1);
    assert.strictEqual(scheduled[0].delay, 7);
    assert.deepStrictEqual(calls, []);
    scheduled[0].callback();
    await flush(60);
    assert.strictEqual(calls[0], 'activity_status');
});

test('interleaved live batch is queued, merged with replay, and caught up by final status', async function() {
    var initial = deferred();
    var statusCalls = 0;
    var harness = makeImmediateController(function(name) {
        if (name === 'activity_status') {
            statusCalls++;
            return statusCalls === 1 ? initial.promise : Promise.resolve(status(SESSION_A, 'capturing', '1', '3'));
        }
        if (name === 'activity_replay') {
            return Promise.resolve(replayPage(SESSION_A, [event(SESSION_A, '1'), event(SESSION_A, '2')], {
                oldest: '1', latest: '3', nextAfter: '2', hasMore: false
            }));
        }
        throw new Error('unexpected command ' + name);
    });
    harness.controller.start();
    await flush();
    harness.handlers.activity_batch_v1(batch(SESSION_A, [event(SESSION_A, '3')]));
    initial.resolve(status(SESSION_A, 'capturing', '1', '2'));
    await flush(60);
    var snapshot = harness.controller.snapshot();
    assert.strictEqual(snapshot.phase, 'live');
    assert.deepStrictEqual(snapshot.events.map(function(value) { return value.sequence; }), ['1', '2', '3']);
    assert.deepStrictEqual(
        harness.eventSnapshots[harness.eventSnapshots.length - 1].map(function(value) { return value.sequence; }),
        ['1', '2', '3'],
        'reconciled canonical events are published to the UI'
    );
});

test('initial replay uses a null exclusive cursor and paginates with exact snake_case DTO fields', async function() {
    var statusCalls = 0;
    var replayCalls = [];
    var harness = makeImmediateController(function(name, args) {
        if (name === 'activity_status') {
            statusCalls++;
            return Promise.resolve(status(SESSION_A, 'capturing', '1', '4'));
        }
        replayCalls.push(args.args);
        if (args.args.after === null) {
            return Promise.resolve(replayPage(SESSION_A, [event(SESSION_A, '1'), event(SESSION_A, '2')], {
                oldest: '1', latest: '4', nextAfter: '2', hasMore: true
            }));
        }
        return Promise.resolve(replayPage(SESSION_A, [event(SESSION_A, '3'), event(SESSION_A, '4')], {
            oldest: '1', latest: '4', nextAfter: '4', hasMore: false
        }));
    });
    harness.controller.start();
    await flush(70);
    assert.strictEqual(statusCalls, 2);
    assert.deepStrictEqual(replayCalls.map(function(value) { return value.after; }), [null, '2']);
    replayCalls.forEach(function(value) {
        assert.strictEqual(value.capture_session, SESSION_A);
        assert.strictEqual(value.max_events, 50);
        assert.strictEqual(value.max_bytes, 65536);
        assert.strictEqual(Object.prototype.hasOwnProperty.call(value, 'captureSession'), false);
    });
    assert.deepStrictEqual(harness.controller.snapshot().events.map(function(value) { return value.sequence; }), ['1', '2', '3', '4']);
});

test('decimal u64 helpers sort and deduplicate above 2^53 through u64 max without numeric coercion', async function() {
    var sequences = controllerLibrary.ActivitySequences;
    assert.strictEqual(sequences.isCanonical('0', true), true);
    assert.strictEqual(sequences.isCanonical('0', false), false);
    assert.strictEqual(sequences.isCanonical('01', true), false);
    assert.strictEqual(sequences.isCanonical('18446744073709551616', true), false);
    assert.strictEqual(sequences.compare('9007199254740993', '9007199254740994'), -1);
    assert.strictEqual(sequences.increment('9007199254740993'), '9007199254740994');
    assert.strictEqual(sequences.increment('18446744073709551614'), '18446744073709551615');
    assert.strictEqual(sequences.increment('18446744073709551615'), null);

    var high = ['9007199254740993', '9007199254740994', '9007199254740995'];
    var statusCalls = 0;
    var harness = makeImmediateController(function(name) {
        if (name === 'activity_status') {
            statusCalls++;
            return Promise.resolve(status(SESSION_A, 'capturing', high[0], high[2]));
        }
        return Promise.resolve(replayPage(SESSION_A, [
            event(SESSION_A, high[2]),
            event(SESSION_A, high[0]),
            event(SESSION_A, high[1]),
            event(SESSION_A, high[1])
        ], { oldest: high[0], latest: high[2], nextAfter: high[2] }));
    });
    harness.controller.start();
    await flush(60);
    assert.deepStrictEqual(harness.controller.snapshot().events.map(function(value) { return value.sequence; }), high);

    var maxCalls = 0;
    var maxHarness = makeImmediateController(function(name) {
        if (name === 'activity_status') {
            maxCalls++;
            return Promise.resolve(status(SESSION_A, 'stopped', '18446744073709551615', '18446744073709551615'));
        }
        return Promise.resolve(replayPage(SESSION_A, [event(SESSION_A, '18446744073709551615')], {
            oldest: '18446744073709551615', latest: '18446744073709551615', nextAfter: '18446744073709551615'
        }));
    });
    maxHarness.controller.start();
    await flush(50);
    assert.strictEqual(maxHarness.controller.snapshot().events[0].sequence, '18446744073709551615');

    var controllerStart = activitySource.indexOf('var ACTIVITY_U64_MAX');
    var controllerEnd = activitySource.indexOf('\nvar activityBootstrap =', controllerStart);
    var controllerSource = activitySource.slice(controllerStart, controllerEnd);
    assert.strictEqual(/\bNumber\b/.test(controllerSource), false);
    assert.strictEqual(controllerSource.indexOf('parse' + 'Int'), -1);
    assert.strictEqual(controllerSource.indexOf('Big' + 'Int'), -1);
    assert.strictEqual(controllerSource.indexOf('big' + 'int'), -1);
});

test('replay gaps recover with a fresh null-cursor resync', async function() {
    var statusCalls = 0;
    var replayCalls = 0;
    var harness = makeImmediateController(function(name, args) {
        if (name === 'activity_status') {
            statusCalls++;
            return Promise.resolve(status(SESSION_A, 'capturing', '1', '2'));
        }
        replayCalls++;
        if (replayCalls === 1) {
            return Promise.resolve(replayPage(SESSION_A, [], { oldest: '1', latest: '2', nextAfter: null, gap: true }));
        }
        assert.strictEqual(args.args.after, null);
        return Promise.resolve(replayPage(SESSION_A, [event(SESSION_A, '1'), event(SESSION_A, '2')], {
            oldest: '1', latest: '2', nextAfter: '2'
        }));
    });
    harness.controller.start();
    await flush(100);
    assert(statusCalls >= 3);
    assert.strictEqual(replayCalls, 2);
    assert.strictEqual(harness.controller.snapshot().phase, 'live');
});

test('an unflagged missing sequence is detected and recovered from a full replay', async function() {
    var replayCalls = 0;
    var harness = makeImmediateController(function(name) {
        if (name === 'activity_status') {
            return Promise.resolve(status(SESSION_A, 'capturing', '1', '3'));
        }
        replayCalls++;
        if (replayCalls === 1) {
            return Promise.resolve(replayPage(SESSION_A, [event(SESSION_A, '1'), event(SESSION_A, '3')], {
                oldest: '1', latest: '3', nextAfter: '3'
            }));
        }
        return Promise.resolve(replayPage(SESSION_A, [event(SESSION_A, '3'), event(SESSION_A, '1'), event(SESSION_A, '2')], {
            oldest: '1', latest: '3', nextAfter: '3'
        }));
    });
    harness.controller.start();
    await flush(120);
    assert(replayCalls >= 2);
    assert.deepStrictEqual(harness.controller.snapshot().events.map(function(value) { return value.sequence; }), ['1', '2', '3']);
});

test('session_mismatch replay and mismatched live batches rebootstrap to the authoritative session', async function() {
    var phase = 0;
    var harness = makeImmediateController(function(name) {
        if (name === 'activity_status') {
            if (phase === 0) return Promise.resolve(status(SESSION_A, 'capturing', '1', '1'));
            return Promise.resolve(status(SESSION_B, 'capturing', '8', '8'));
        }
        if (phase === 0) {
            phase = 1;
            return Promise.resolve({ result: 'session_mismatch', status: status(SESSION_B, 'capturing', '8', '8') });
        }
        return Promise.resolve(replayPage(SESSION_B, [event(SESSION_B, '8')], {
            oldest: '8', latest: '8', nextAfter: '8'
        }));
    });
    harness.controller.start();
    await flush(100);
    assert.strictEqual(harness.controller.snapshot().captureSession, SESSION_B);
    assert.deepStrictEqual(harness.controller.snapshot().events.map(function(value) { return value.sequence; }), ['8']);

    var switched = false;
    var liveHarness = makeImmediateController(function(name) {
        if (name === 'activity_status') {
            return Promise.resolve(switched
                ? status(SESSION_B, 'capturing', '5', '5')
                : status(SESSION_A, 'capturing', '1', '1'));
        }
        var target = switched ? SESSION_B : SESSION_A;
        var sequence = switched ? '5' : '1';
        return Promise.resolve(replayPage(target, [event(target, sequence)], {
            oldest: sequence, latest: sequence, nextAfter: sequence
        }));
    });
    liveHarness.controller.start();
    await flush(60);
    switched = true;
    liveHarness.handlers.activity_batch_v1(batch(SESSION_B, [event(SESSION_B, '5')]));
    await flush(80);
    assert.strictEqual(liveHarness.controller.snapshot().captureSession, SESSION_B);
});

test('identity transition invalidates stale replay, clears state, sends no Stop, and switched/error rebootstrap', async function() {
    var oldReplay = deferred();
    var currentSession = SESSION_A;
    var replayCount = 0;
    var harness = makeImmediateController(function(name) {
        if (name === 'activity_status') {
            var sequence = currentSession === SESSION_A ? '1' : '9';
            return Promise.resolve(status(currentSession, 'capturing', sequence, sequence));
        }
        replayCount++;
        if (replayCount === 1) return oldReplay.promise;
        var sequence = currentSession === SESSION_A ? '1' : '9';
        return Promise.resolve(replayPage(currentSession, [event(currentSession, sequence)], {
            oldest: sequence, latest: sequence, nextAfter: sequence
        }));
    });
    harness.controller.start();
    await flush(30);
    harness.handlers.identity_switching({ generation: '7' });
    assert.strictEqual(harness.controller.snapshot().phase, 'identity_transition');
    assert.strictEqual(harness.controller.snapshot().events.length, 0);
    assert.strictEqual(harness.identityTransitions(), 1);
    assert.strictEqual(harness.invokeCalls.some(function(call) { return call.name === 'activity_stop'; }), false);

    currentSession = SESSION_B;
    harness.handlers.identity_switched({ generation: '7' });
    await flush(70);
    oldReplay.resolve(replayPage(SESSION_A, [event(SESSION_A, '1')], { oldest: '1', latest: '1', nextAfter: '1' }));
    await flush(40);
    assert.strictEqual(harness.controller.snapshot().captureSession, SESSION_B);
    assert.deepStrictEqual(harness.controller.snapshot().events.map(function(value) { return value.sequence; }), ['9']);

    var before = harness.invokeCalls.filter(function(call) { return call.name === 'activity_status'; }).length;
    harness.handlers.identity_error({});
    await flush(60);
    var after = harness.invokeCalls.filter(function(call) { return call.name === 'activity_status'; }).length;
    assert(after > before, 'identity_error must rebootstrap');
});

test('identity transition quarantines delayed status and events until its hard-reset fence', async function() {
    var current = status(SESSION_A, 'capturing', '1', '1', 'normal', '1');
    var harness = makeImmediateController(function(name) {
        if (name === 'activity_status') return Promise.resolve(current);
        return Promise.resolve(replayPage(SESSION_A, [event(SESSION_A, '1')], {
            oldest: '1', latest: '1', nextAfter: '1'
        }));
    });
    harness.controller.start();
    await flush(60);
    var statusCount = harness.statuses.length;

    harness.handlers.identity_switching({ generation: '2' });
    harness.handlers.activity_status_v1(
        status(SESSION_A, 'capturing', '1', '1', 'normal', '1')
    );

    assert.strictEqual(harness.controller.snapshot().phase, 'identity_transition');
    assert.strictEqual(harness.controller.snapshot().events.length, 0);
    assert.strictEqual(harness.controller.snapshot().identityQuarantine, true);
    assert.strictEqual(harness.statuses.length, statusCount);

    var off = status(null, 'off', null, null, null, '2');
    harness.handlers.activity_boundary_v1({
        version: 1,
        kind: 'hard_reset',
        identity_generation: '2',
        capture_generation: '2',
        status: off
    });
    assert.strictEqual(harness.controller.snapshot().phase, 'identity_transition');
    assert.strictEqual(harness.controller.snapshot().identityQuarantine, true);
});

test('post-switch quarantine survives phase changes until authoritative status resolves', async function() {
    var postSwitchStatus = deferred();
    var statusCalls = 0;
    var harness = makeImmediateController(function(name) {
        if (name === 'activity_status') {
            statusCalls++;
            if (statusCalls === 1) {
                return Promise.resolve(status(SESSION_A, 'capturing', '1', '1', 'normal', '1'));
            }
            return postSwitchStatus.promise;
        }
        return Promise.resolve(replayPage(SESSION_A, [event(SESSION_A, '1')], {
            oldest: '1', latest: '1', nextAfter: '1'
        }));
    });
    harness.controller.start();
    await flush(60);

    harness.handlers.identity_switching({ generation: '2' });
    harness.handlers.identity_switched({ generation: '2' });
    await flush(20);
    assert.strictEqual(harness.controller.snapshot().phase, 'status');
    assert.strictEqual(harness.controller.snapshot().identityQuarantine, true);

    var acceptedBefore = harness.statuses.length;
    harness.handlers.activity_status_v1(
        status(SESSION_A, 'capturing', '1', '1', 'normal', '1')
    );
    assert.strictEqual(harness.statuses.length, acceptedBefore);
    assert.strictEqual(harness.controller.snapshot().identityQuarantine, true);

    postSwitchStatus.resolve(status(null, 'off', null, null, null, '2'));
    await flush(60);
    assert.strictEqual(harness.controller.snapshot().identityQuarantine, false);
});

test('queued live data is bounded and overflow recovers from the backend ring', async function() {
    var firstStatus = deferred();
    var statusCalls = 0;
    var replayCalls = 0;
    var harness = makeImmediateController(function(name) {
        if (name === 'activity_status') {
            statusCalls++;
            if (statusCalls === 1) return firstStatus.promise;
            return Promise.resolve(status(SESSION_A, 'capturing', '2', '3'));
        }
        replayCalls++;
        if (replayCalls === 1) {
            return Promise.resolve(replayPage(SESSION_A, [event(SESSION_A, '1'), event(SESSION_A, '2'), event(SESSION_A, '3')], {
                oldest: '1', latest: '3', nextAfter: '3'
            }));
        }
        return Promise.resolve(replayPage(SESSION_A, [event(SESSION_A, '2'), event(SESSION_A, '3')], {
            oldest: '2', latest: '3', nextAfter: '3'
        }));
    }, { maxEvents: 2 });
    harness.controller.start();
    await flush(25);
    harness.handlers.activity_batch_v1(batch(SESSION_A, [event(SESSION_A, '1'), event(SESSION_A, '2'), event(SESSION_A, '3')]));
    assert.strictEqual(harness.controller.snapshot().queuedLiveCount, 2);
    assert.strictEqual(harness.controller.snapshot().queueOverflow, true);
    firstStatus.resolve(status(SESSION_A, 'capturing', '1', '3'));
    await flush(120);
    assert.strictEqual(harness.controller.snapshot().phase, 'live');
    assert.deepStrictEqual(harness.controller.snapshot().events.map(function(value) { return value.sequence; }), ['2', '3']);
    assert(replayCalls >= 2, 'overflow must trigger replay recovery');
});

test('a late batch cannot enter an Off live session and instead forces authoritative status recovery', async function() {
    var capturing = false;
    var harness = makeImmediateController(function(name) {
        if (name === 'activity_status') {
            return Promise.resolve(capturing
                ? status(SESSION_A, 'capturing', '7', '7')
                : status(null, 'off', null, null, null));
        }
        return Promise.resolve(replayPage(SESSION_A, [event(SESSION_A, '7')], {
            oldest: '7', latest: '7', nextAfter: '7'
        }));
    });
    harness.controller.start();
    await flush(50);
    assert.strictEqual(harness.controller.snapshot().captureSession, null);
    capturing = true;
    harness.handlers.activity_batch_v1(batch(SESSION_A, [event(SESSION_A, '7')]));
    await flush(80);
    assert.strictEqual(harness.controller.snapshot().captureSession, SESSION_A);
    assert.deepStrictEqual(harness.controller.snapshot().events.map(function(value) { return value.sequence; }), ['7']);
});

test('a delayed pre-Clear batch cannot reintroduce rows below the acknowledged oldest fence', async function() {
    var currentStatus = status(SESSION_A, 'capturing', '1', '3', 'normal', '1');
    var harness = makeImmediateController(function(name) {
        if (name === 'activity_status') return Promise.resolve(currentStatus);
        if (currentStatus.ingress_generation === '1') {
            return Promise.resolve(replayPage(SESSION_A, [
                event(SESSION_A, '1'), event(SESSION_A, '2'), event(SESSION_A, '3')
            ], { oldest: '1', latest: '3', nextAfter: '3' }));
        }
        return Promise.resolve(replayPage(SESSION_A, [
            event(SESSION_A, '4', 'diagnostics.capture_cleared', '2')
        ], { oldest: '4', latest: '4', nextAfter: '4' }));
    });
    harness.controller.start();
    await flush(70);
    assert.deepStrictEqual(
        harness.controller.snapshot().events.map(function(value) { return value.sequence; }),
        ['1', '2', '3']
    );

    currentStatus = status(SESSION_A, 'capturing', '4', '4', 'normal', '2');
    harness.handlers.activity_status_v1(currentStatus);
    await flush(100);
    assert.deepStrictEqual(
        harness.controller.snapshot().events.map(function(value) { return value.sequence; }),
        ['4']
    );

    harness.handlers.activity_batch_v1(batch(SESSION_A, [event(SESSION_A, '2')]));
    await flush(20);
    assert.deepStrictEqual(
        harness.controller.snapshot().events.map(function(value) { return value.sequence; }),
        ['4'],
        'late delivery from the cleared range must stay discarded'
    );
});

test('worker recovery status clears the old session and resynchronizes to Off', async function() {
    var currentStatus = status(SESSION_A, 'capturing', '1', '1', 'normal', '1', '1');
    var harness = makeImmediateController(function(name) {
        if (name === 'activity_status') return Promise.resolve(currentStatus);
        return Promise.resolve(replayPage(SESSION_A, [event(SESSION_A, '1')], {
            oldest: '1', latest: '1', nextAfter: '1'
        }));
    });
    harness.controller.start();
    await flush(60);
    assert.strictEqual(harness.controller.snapshot().captureSession, SESSION_A);

    currentStatus = status(null, 'off', null, null, null, '2', '2');
    currentStatus.worker_state = 'recovered';
    harness.handlers.activity_status_v1(currentStatus);
    await flush(80);
    assert.strictEqual(harness.controller.snapshot().phase, 'live');
    assert.strictEqual(harness.controller.snapshot().captureSession, null);
    assert.deepStrictEqual(harness.controller.snapshot().events, []);
});

test('capturing and stopped reloads adopt backend status without an implicit disable', async function() {
    async function run(stateName) {
        var seen = [];
        var harness = makeImmediateController(function(name) {
            if (name === 'activity_status') {
                return Promise.resolve(status(SESSION_A, stateName, '1', '1'));
            }
            return Promise.resolve(replayPage(SESSION_A, [event(SESSION_A, '1')], {
                oldest: '1', latest: '1', nextAfter: '1'
            }));
        });
        harness.controller.start();
        await flush(60);
        seen = harness.statuses;
        assert.strictEqual(seen[seen.length - 1].state, stateName);
        assert.strictEqual(harness.invokeCalls.some(function(call) { return call.name === 'enable_network_log'; }), false);
    }
    await run('capturing');
    await run('stopped');
    assert.strictEqual(activitySource.indexOf("enabled: false"), -1);
});

test('foreground notification fires only after successful backend lifecycle handling, including BFCache metadata', async function() {
    var start = stateSource.indexOf('function _postLifecycleForeground');
    var end = stateSource.indexOf('\nfunction _currentLifecycleForeground', start);
    assert(start !== -1 && end !== -1, 'lifecycle function source markers must exist');
    var calls = [];
    var gates = [];
    var document = {
        dispatchEvent: function(value) { calls.push(value); }
    };
    var context = {
        RS: {
            invoke: function() {
                var gate = deferred();
                gates.push(gate);
                return gate.promise;
            }
        },
        document: document,
        CustomEvent: function(name, options) { this.type = name; this.detail = options.detail; },
        Promise: Promise
    };
    vm.runInNewContext(stateSource.slice(start, end), context, { filename: 'state-lifecycle.js' });

    var success = context._postLifecycleForeground(true, { source: 'pageshow', persisted: true });
    assert.deepStrictEqual(calls, []);
    gates[0].resolve();
    assert.strictEqual(await success, true);
    assert.strictEqual(calls.length, 1);
    assert.strictEqual(calls[0].type, 'rs-lifecycle-foreground-handled');
    assert.strictEqual(calls[0].detail.persisted, true);

    var failure = context._postLifecycleForeground(true, { source: 'lifecycle', persisted: false });
    gates[1].reject(new Error('backend unavailable'));
    assert.strictEqual(await failure, false);
    assert.strictEqual(calls.length, 1, 'failed lifecycle fence must not trigger Activity resync');
    assert(stateSource.indexOf('if (handled) _refreshAfterResume();') !== -1);
    assert(stateSource.indexOf("{ source: 'pageshow', persisted: true }") !== -1);
    assert(activitySource.indexOf("isTauriMobile()") !== -1);
    assert(activitySource.indexOf("detail.persisted") !== -1);
});

function FakeElement(id, dataProfile) {
    this.id = id;
    this.style = {};
    this.attributes = {};
    this.disabled = false;
    this.checked = false;
    this.value = '';
    this.textContent = '';
    this.innerHTML = '';
    this.children = [];
    this.scrollHeight = 0;
    this.scrollTop = 0;
    this.clientHeight = 0;
    this.isConnected = true;
    this.focusCount = 0;
    this.dataProfile = dataProfile || null;
    this.parentElement = null;
    var classes = Object.create(null);
    this.classList = {
        add: function(name) { classes[name] = true; },
        remove: function(name) { delete classes[name]; },
        contains: function(name) { return !!classes[name]; }
    };
}

FakeElement.prototype.setAttribute = function(name, value) { this.attributes[name] = String(value); };
FakeElement.prototype.removeAttribute = function(name) { delete this.attributes[name]; };
FakeElement.prototype.getAttribute = function(name) {
    if (name === 'data-profile') return this.dataProfile;
    return this.attributes[name] == null ? null : this.attributes[name];
};
FakeElement.prototype.addEventListener = function() {};
FakeElement.prototype.querySelectorAll = function() { return []; };
FakeElement.prototype.querySelector = function() { return null; };
FakeElement.prototype.focus = function() { this.focusCount++; };

function loadUiHarness(options) {
    options = options || {};
    var end = activitySource.indexOf('\nvar REASON_LABELS');
    assert(end !== -1, 'Activity UI source boundary must exist');
    var ids = {};
    ['activity-enable-btn', 'activity-capture-btn', 'activity-clear-btn', 'activity-privacy-gate',
        'activity-active', 'activity-feed', 'activity-filters', 'activity-status',
        'activity-status-label', 'activity-result-count', 'activity-search-input'].forEach(function(id) {
        ids[id] = new FakeElement(id);
    });
    var profiles = ['normal', 'trace'].map(function(profile) {
        return new FakeElement('profile-' + profile, profile);
    });
    var documentHandlers = {};
    var document = {
        activeElement: ids['activity-capture-btn'],
        getElementById: function(id) { return ids[id] || null; },
        querySelectorAll: function(selector) { return selector === '.activity-profile-btn' ? profiles : []; },
        querySelector: function() { return null; },
        createElement: function(id) { return new FakeElement(id); },
        addEventListener: function(name, handler) { documentHandlers[name] = handler; }
    };
    var invoke = function() { return Promise.reject(new Error('unconfigured invoke')); };
    var window = { RS: { diag: function() {} } };
    window.RS.invoke = function(name, args) { return invoke(name, args); };
    window.RS.listen = function() { return Promise.resolve(function() {}); };
    var context = {
        window: window,
        RS: window.RS,
        document: document,
        navigator: { maxTouchPoints: 0 },
        Promise: Promise,
        JSON: JSON,
        Object: Object,
        Array: Array,
        String: String,
        Date: Date,
        isMobile: function() { return false; },
        isTauriMobile: function() { return !!options.mobile; },
        setTimeout: setTimeout,
        clearTimeout: clearTimeout,
        requestAnimationFrame: function(callback) { callback(); },
        escapeHtml: function(value) { return String(value); },
        showToast: function() {},
        _use12Hour: false
    };
    vm.runInNewContext(activitySource.slice(0, end), context, { filename: 'activity-ui.js' });
    var resyncs = [];
    context.activityBootstrap = {
        forceResync: function(reason) { resyncs.push(reason); },
        resync: function(reason) { resyncs.push(reason); },
        start: function() {}
    };
    return {
        context: context,
        ids: ids,
        profiles: profiles,
        documentHandlers: documentHandlers,
        resyncs: resyncs,
        setInvoke: function(value) { invoke = value; }
    };
}

test('successful foreground notifications resync every Tauri mobile runtime and persisted BFCache pages', function() {
    var mobile = loadUiHarness({ mobile: true });
    mobile.context.initActivity();
    mobile.documentHandlers['rs-lifecycle-foreground-handled']({ detail: { persisted: false } });
    assert(mobile.resyncs.indexOf('mobile_foreground') !== -1);

    var bfcache = loadUiHarness({ mobile: false });
    bfcache.context.initActivity();
    bfcache.documentHandlers['rs-lifecycle-foreground-handled']({ detail: { persisted: true } });
    assert(bfcache.resyncs.indexOf('pageshow_persisted') !== -1);
});

test('pending controls are tokened, disabled/aria-busy, roll back with status reconciliation, and preserve focus', async function() {
    var ui = loadUiHarness();
    var ctx = ui.context;
    var capturing = status(SESSION_A, 'capturing', '1', '1');
    var stopped = status(SESSION_A, 'stopped', '1', '1');
    ctx.applyActivityStatus(capturing);
    ctx.activityEvents = [event(SESSION_A, '1')];

    var stopGate = deferred();
    ui.setInvoke(function(name) {
        assert.strictEqual(name, 'activity_stop');
        return stopGate.promise;
    });
    var stop = ctx.setActivityCapture(false);
    assert.strictEqual(ctx.activityEvents.length, 1);
    assert.strictEqual(ui.ids['activity-capture-btn'].disabled, true);
    assert.strictEqual(ui.ids['activity-capture-btn'].attributes['aria-busy'], 'true');
    stopGate.resolve(stopped);
    await stop;
    assert.strictEqual(ctx.activityCaptureState, 'stopped');
    assert.strictEqual(ctx.activityEvents.length, 1, 'Pause retains visible rows');
    assert.strictEqual(ui.ids['activity-active'].style.display, '');
    assert.strictEqual(ui.ids['activity-capture-btn'].textContent, 'Resume');
    assert.strictEqual(ui.ids['activity-capture-btn'].disabled, false);
    assert.strictEqual(ui.ids['activity-capture-btn'].attributes['aria-busy'], undefined);
    assert(ui.ids['activity-capture-btn'].focusCount > 0);

    ctx.applyActivityStatus(capturing);
    var invokeCount = 0;
    ui.setInvoke(function(name) {
        invokeCount++;
        if (invokeCount === 1) return Promise.reject(new Error('control failed'));
        assert.strictEqual(name, 'activity_status');
        return Promise.resolve(capturing);
    });
    await ctx.setActivityCapture(false);
    assert.strictEqual(ctx.activityCaptureState, 'capturing');
    assert.strictEqual(ctx.activityEvents.length, 1);
    assert.strictEqual(ui.ids['activity-capture-btn'].disabled, false);
    assert(ui.resyncs.indexOf('control_status_reconciled') !== -1);
});

test('Activity title shows status only for live and paused sessions', function() {
    var ui = loadUiHarness();
    var ctx = ui.context;
    var statusElement = ui.ids['activity-status'];
    var statusLabel = ui.ids['activity-status-label'];

    ctx.activityCaptureState = 'off';
    ctx.updateActivityUI();
    assert.strictEqual(statusElement.hidden, true);
    assert.strictEqual(statusLabel.textContent, '');

    ctx.applyActivityStatus(status(SESSION_A, 'capturing', '1', '1'));
    assert.strictEqual(statusElement.hidden, false);
    assert.strictEqual(statusLabel.textContent, 'Live');

    ctx.applyActivityStatus(status(SESSION_A, 'stopped', '1', '1'));
    assert.strictEqual(statusElement.hidden, false);
    assert.strictEqual(statusLabel.textContent, 'Paused');
});

test('Clear waits for acknowledgement, clears only on success, and failure retains rows', async function() {
    var ui = loadUiHarness();
    var ctx = ui.context;
    var stopped = status(SESSION_A, 'stopped', '2', '2');
    ctx.applyActivityStatus(stopped);
    ctx.activityEvents = [event(SESSION_A, '2')];
    var clearGate = deferred();
    ui.setInvoke(function(name) {
        assert.strictEqual(name, 'activity_clear');
        return clearGate.promise;
    });
    var clearing = ctx.clearActivity();
    assert.strictEqual(ctx.activityEvents.length, 1);
    assert.strictEqual(ui.ids['activity-clear-btn'].disabled, true);
    clearGate.resolve(stopped);
    await clearing;
    assert.strictEqual(ctx.activityEvents.length, 0);
    assert(ui.resyncs.indexOf('clear_acknowledged') !== -1);

    ctx.activityEvents = [event(SESSION_A, '2')];
    var calls = 0;
    ui.setInvoke(function(name) {
        calls++;
        if (calls === 1) return Promise.reject(new Error('clear failed'));
        assert.strictEqual(name, 'activity_status');
        return Promise.resolve(stopped);
    });
    await ctx.clearActivity();
    assert.strictEqual(ctx.activityEvents.length, 1);
    assert.strictEqual(ui.ids['activity-clear-btn'].disabled, false);
});

test('Normal and Trace controls call the typed profile command and Resume uses the typed capture command', async function() {
    var ui = loadUiHarness();
    var ctx = ui.context;
    ctx.applyActivityStatus(status(SESSION_A, 'capturing', '1', '1'));
    var profiles = [];
    ui.setInvoke(function(name, args) {
        assert.strictEqual(name, 'activity_set_profile');
        profiles.push(args.args.profile);
        return Promise.resolve(status(SESSION_A, 'capturing', '1', '1', args.args.profile));
    });
    await ctx.setActivityProfile('trace');
    await ctx.setActivityProfile('normal');
    assert.deepStrictEqual(profiles, ['trace', 'normal']);
    assert.strictEqual(ctx.activityProfile, 'normal');
    assert.strictEqual(ui.profiles[0].attributes['aria-pressed'], 'true');

    ctx.applyActivityStatus(status(SESSION_A, 'stopped', '1', '1', 'trace'));
    assert.strictEqual(ctx.activityProfile, 'trace');
    ui.setInvoke(function(name) {
        assert.strictEqual(name, 'activity_resume');
        return Promise.resolve(status(SESSION_A, 'capturing', '1', '2', 'normal'));
    });
    await ctx.setActivityCapture(true);
    assert.strictEqual(ctx.activityProfile, 'normal');
});

test('canonical channel events render as readable, filterable rows with privacy-gated details', function() {
    var ui = loadUiHarness();
    var ctx = ui.context;
    var channelEvent = event(SESSION_A, '7', 'channels.room.join_requested');
    channelEvent.area = 'channels';
    channelEvent.kind = 'channels.room.join_requested';
    channelEvent.summary_code = 'channels.room.join_requested';
    channelEvent.outcome = 'started';
    channelEvent.timestamp_unix_ms = 1700000000000;
    channelEvent.attributes = [{
        key: 'hub',
        value: {
            type: 'identifier',
            value: { kind: 'hub', pseudonym: 'masked-hub-token-123456', ordinal: 2 }
        }
    }];
    ctx.activityEvents = [channelEvent];
    ctx.renderActivityFilters();
    assert(ui.ids['activity-filters'].innerHTML.indexOf('Channels') !== -1);
    assert(ui.ids['activity-filters'].innerHTML.indexOf('Links') === -1);

    ctx.renderActivityFeed();
    assert(ui.ids['activity-feed'].innerHTML.indexOf('Channel join requested') !== -1);
    assert(ui.ids['activity-feed'].innerHTML.indexOf('data-area="channels"') !== -1);
    assert(ui.ids['activity-feed'].innerHTML.indexOf('masked-hub-token-123456') === -1);

    ctx.activityExpandedSequence = '7';
    ctx.renderActivityFeed();
    assert(ui.ids['activity-feed'].innerHTML.indexOf('channels.room.join_requested') !== -1);
    assert(ui.ids['activity-feed'].innerHTML.indexOf('<code>Reveal</code>') !== -1);
    assert(ui.ids['activity-feed'].innerHTML.indexOf('data-action="reveal"') !== -1);
    assert(ui.ids['activity-feed'].innerHTML.indexOf('data-icon="reveal"') !== -1);
    assert(ui.ids['activity-feed'].innerHTML.indexOf('data-icon="copy"') === -1);
    assert(ui.ids['activity-feed'].innerHTML.indexOf('data-field="hub"') !== -1);
    assert(ui.ids['activity-feed'].innerHTML.indexOf('masked-hub-token-123456') === -1);
});

test('remote channel closes explain the authenticated teardown without guessing a cause', function() {
    var ui = loadUiHarness();
    var ctx = ui.context;
    var closeEvent = event(SESSION_A, '8', 'channels.session.closed');
    closeEvent.area = 'channels';
    closeEvent.kind = 'channels.session.closed';
    closeEvent.summary_code = 'channels.session.closed';
    closeEvent.direction = 'inbound';
    closeEvent.outcome = 'failed';
    closeEvent.attributes = [{
        key: 'hub',
        value: {
            type: 'identifier',
            value: { kind: 'hub', pseudonym: 'masked-hub-token', ordinal: 2 }
        }
    }, {
        key: 'reason',
        value: { type: 'code', value: 'remote' }
    }, {
        key: 'link',
        value: {
            type: 'identifier',
            value: { kind: 'link', pseudonym: 'masked-link-token', ordinal: 4 }
        }
    }, {
        key: 'duration_ms',
        value: { type: 'unsigned', value: 321000 }
    }];
    ctx.activityEvents = [closeEvent];
    ctx.activityExpandedSequence = '8';
    ctx.renderActivityFeed();

    var html = ui.ids['activity-feed'].innerHTML;
    assert(html.indexOf('Hub ended channel session') !== -1);
    assert(html.indexOf('Remote Link teardown') !== -1);
    assert(html.indexOf('authenticated Reticulum Link') !== -1);
    assert(html.indexOf('carries no detailed reason') !== -1);
    assert(html.indexOf('321000 ms') !== -1);
    assert(html.indexOf('Inbound') !== -1);
    assert(html.indexOf('data-field="link"') !== -1);
});

test('identifier controls reveal first and copy only after disclosure', async function() {
    var ui = loadUiHarness();
    var ctx = ui.context;
    var rawAddress = '00112233445566778899aabbccddeeff';
    var announceEvent = event(SESSION_A, '9', 'rns.announce.observed');
    announceEvent.area = 'network';
    announceEvent.kind = 'rns.announce.observed';
    announceEvent.summary_code = 'rns.announce.observed';
    announceEvent.timestamp_unix_ms = 1700000000000;
    announceEvent.attributes = [{
        key: 'destination',
        value: {
            type: 'identifier',
            value: { kind: 'destination', pseudonym: 'masked-destination', ordinal: 4 }
        }
    }, {
        key: 'hops',
        value: { type: 'unsigned', value: 2 }
    }];
    ctx.activityEvents = [announceEvent];
    ctx.activityExpandedSequence = '9';
    ctx.PeersCache = {
        get: function(hash) {
            assert.strictEqual(hash, rawAddress);
            return {
                hash: hash,
                display_name: 'Alice',
                services: ['lxmf.delivery']
            };
        }
    };
    ctx.announceCache = [{ hash: rawAddress, display_name: 'Alice' }];
    var revealCalls = 0;
    ui.setInvoke(function(name, args) {
        revealCalls++;
        assert.strictEqual(name, 'activity_reveal');
        assert.strictEqual(args.args.capture_session, SESSION_A);
        assert.strictEqual(args.args.sequence, '9');
        assert.strictEqual(args.args.field, 'destination');
        return Promise.resolve({
            result: 'identifier',
            key: 'destination',
            kind: 'destination',
            value: rawAddress
        });
    });

    ctx.renderActivityFeed();
    var hiddenHtml = ui.ids['activity-feed'].innerHTML;
    assert(hiddenHtml.indexOf('<code>Reveal</code>') !== -1);
    assert(hiddenHtml.indexOf('data-action="reveal"') !== -1);
    assert(hiddenHtml.indexOf('data-icon="reveal"') !== -1);

    var copied = null;
    ctx.RS.copyText = function(value) {
        copied = value;
        return Promise.resolve(true);
    };
    assert.strictEqual(await ctx.copyActivityIdentifier('9', 'destination'), false);
    assert.strictEqual(await ctx.activateActivityIdentifier('9', 'destination'), true);
    assert.strictEqual(copied, null, 'the reveal action must not copy');

    ctx.renderActivityFeed();
    var html = ui.ids['activity-feed'].innerHTML;
    assert(html.indexOf('Alice announced') !== -1);
    assert(html.indexOf('activity-identity-name">Alice') !== -1);
    assert(html.indexOf('LXMF address') !== -1);
    assert(html.indexOf('001122334…eeff') !== -1);
    assert(html.indexOf(rawAddress) === -1, 'full address must not be painted into the Activity DOM');
    assert(html.indexOf('data-action="copy"') !== -1);
    assert(html.indexOf('data-icon="copy"') !== -1);
    assert(html.indexOf('data-icon="reveal"') === -1);

    assert.strictEqual(await ctx.activateActivityIdentifier('9', 'destination'), true);
    assert.strictEqual(copied, rawAddress);
    assert.strictEqual(revealCalls, 1, 'copy should reuse the prior explicit reveal');
});

test('identity UI reset clears canonical visible state without issuing a Stop command', async function() {
    var ui = loadUiHarness();
    var ctx = ui.context;
    ctx.activityProfile = 'trace';
    ctx.activityCaptureState = 'capturing';
    ctx.activityEvents = [event(SESSION_A, '1')];
    var commands = [];
    ctx.RS.invoke = function(name) { commands.push(name); return Promise.resolve(); };
    ctx.activityBootstrap = controllerLibrary.createActivityBootstrap({
        listen: function() { return Promise.resolve(function() {}); },
        invoke: function(name) { commands.push(name); return Promise.resolve(status(null, 'off', null, null, null)); },
        onIdentityTransition: function() {
            ctx._activityControlToken += 1;
            ctx._activityControlPending = false;
            ctx.activityStatus = null;
            ctx.activityCaptureState = 'off';
            ctx.activityProfile = 'normal';
            ctx.activityEvents = [];
        },
        retryDelays: [],
        diagnose: function() {}
    });
    ctx.activityBootstrap.identityTransition();
    assert.strictEqual(ctx.activityProfile, 'normal');
    assert.strictEqual(ctx.activityEvents.length, 0);
    assert.strictEqual(commands.indexOf('activity_stop'), -1);
});

test('typed batches publish canonical events without a legacy network_event listener', async function() {
    var statusCalls = 0;
    var harness = makeImmediateController(function(name) {
        if (name === 'activity_status') {
            statusCalls++;
            return Promise.resolve(status(SESSION_A, 'capturing', '1', '1'));
        }
        return Promise.resolve(replayPage(SESSION_A, [event(SESSION_A, '1')], {
            oldest: '1', latest: '1', nextAfter: '1'
        }));
    });
    harness.controller.start();
    await flush(60);
    harness.handlers.activity_batch_v1(batch(SESSION_A, [event(SESSION_A, '1')]));
    await flush();
    assert.deepStrictEqual(
        harness.eventSnapshots[harness.eventSnapshots.length - 1].map(function(value) { return value.sequence; }),
        ['1']
    );
    assert.strictEqual(activitySource.indexOf("RS.listen('network_event'"), -1);
    assert.strictEqual(activitySource.indexOf("RS.listen('network_log_level_changed'"), -1);
});

test('Activity state stays session-only and the transformed controls are explicit and accessible', function() {
    assert.strictEqual(activitySource.indexOf('localStorage'), -1);
    assert.strictEqual(activitySource.indexOf('sessionStorage'), -1);
    assert.strictEqual(activitySource.indexOf('enable_network_log'), -1);
    assert.strictEqual(activitySource.indexOf('set_network_log_level'), -1);
    assert(indexSource.indexOf('id="activity-capture-btn"') !== -1);
    assert(indexSource.indexOf('data-profile="normal" aria-pressed="true"') !== -1);
    assert(indexSource.indexOf('data-profile="trace" aria-pressed="false"') !== -1);
    assert(indexSource.indexOf('id="activity-search-input" type="search"') !== -1);
    assert(indexSource.indexOf('autocapitalize="off"') !== -1);
    assert(indexSource.indexOf('role="log" aria-live="polite"') !== -1);
    assert.strictEqual(indexSource.indexOf('activity-status-dot'), -1);
    assert(indexSource.indexOf('id="activity-status" data-state="off" hidden') !== -1);
    assert.strictEqual(indexSource.indexOf('id="activity-status-label">Off'), -1);
    assert(activitySource.indexOf("? 'Live'") !== -1);
    assert.strictEqual(activitySource.indexOf("? 'Recording'"), -1);
});

test('sampled observations are summarized without presenting them as failures', function() {
    var sampled = {
        kind: 'diagnostics.sampled',
        summary_code: 'diagnostics.sampled',
        attributes: [
            { key: 'sampled_count', value: { type: 'unsigned', value: 11 } },
            { key: 'source_area', value: { type: 'code', value: 'network' } },
            { key: 'reason', value: { type: 'code', value: 'sustained_rate_limit' } },
            { key: 'time_span_ms', value: { type: 'unsigned', value: 0 } }
        ]
    };
    assert.strictEqual(
        presentationLibrary.summary(sampled),
        '11 Network observations summarized'
    );
    assert.strictEqual(presentationLibrary.label('sampled_count'), 'Summarized');
    assert.strictEqual(
        presentationLibrary.format(sampled.attributes[1]),
        'Network'
    );
    assert.strictEqual(
        presentationLibrary.format(sampled.attributes[2]),
        'High-volume activity'
    );
    assert.strictEqual(presentationLibrary.area(sampled), 'network');

    var dropped = {
        kind: 'diagnostics.dropped',
        summary_code: 'diagnostics.dropped',
        attributes: [
            { key: 'dropped_count', value: { type: 'unsigned', value: 1 } }
        ]
    };
    assert.strictEqual(
        presentationLibrary.summary(dropped),
        '1 Activity entry was not recorded'
    );
});

(async function run() {
    var failures = 0;
    for (var i = 0; i < tests.length; i++) {
        try {
            await tests[i].fn();
            console.log('  ok  ' + tests[i].name);
        } catch (error) {
            failures++;
            console.error('FAIL  ' + tests[i].name);
            console.error(error && error.stack ? error.stack : error);
        }
    }
    if (failures) {
        console.error(failures + ' failure(s)');
        process.exit(1);
    }
    console.log('all ' + tests.length + ' Activity bootstrap checks passed');
})();
