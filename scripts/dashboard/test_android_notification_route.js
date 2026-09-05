#!/usr/bin/env node
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const root = path.join(__dirname, '..', '..', 'dashboard');
const events = fs.readFileSync(path.join(root, 'static/js/tauri_events.js'), 'utf8');
const games = fs.readFileSync(path.join(root, 'static/js/games_tab.js'), 'utf8');
function range(source, start, end) {
    const from = source.indexOf(start);
    const to = source.indexOf(end, from);
    assert(from >= 0 && to > from);
    return source.slice(from, to);
}
const routeCode = range(events, 'function _decodeChannelNotificationRoute', '\nfunction _initNotificationTapRouting');
const gameCode = range(games, '    window.openGameSession = function', '\n    window.updateGamesBadge');
const flush = () => new Promise(resolve => setImmediate(resolve));
function deferred() {
    let resolve, reject;
    const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
    return { promise, resolve, reject };
}
function dashboard() {
    const classes = new Set();
    const controls = { now: 0, locked: false, ready: true, needsSetup: false, overlay: false, wakes: 0 };
    const timers = [];
    const calls = [];
    const opens = [];
    const context = {
        Promise, Uint8Array, TextDecoder,
        document: {
            readyState: 'complete', hidden: false,
            body: { classList: { contains: value => classes.has(value) } },
            getElementById: () => controls.overlay ? {} : null,
            addEventListener: () => assert.fail('Loaded document must wake immediately')
        },
        performance: { now: () => controls.now },
        setTimeout: callback => { timers.push(callback); },
        _navInitialLoad: false, _navTransitioning: false,
        RS: { invoke: command => {
            calls.push(command);
            return Promise.resolve(command === 'api_startup_progress'
                ? { stage: controls.ready ? 'ready' : 'checking', hw_locked: controls.locked }
                : { needs_setup: controls.needsSetup });
        } },
        window: { RatspeakAndroid: { notificationDashboardReady: () => { controls.wakes++; } } },
        openConversationWith: id => opens.push(id)
    };
    vm.runInNewContext(routeCode, context, { filename: 'android-notification-route.js' });
    return { context, controls, classes, calls, opens, timers,
        receive: (route = 'lxmf:abc', token = '1', remaining = 30000) =>
            context._receiveAndroidNotificationRoute(route, token, remaining) };
}

async function main() {
    {
        const d = dashboard();
        d.context._navInitialLoad = true;
        assert.equal(d.receive(), false);
        d.context._navInitialLoad = false;
        d.classes.add('checking-setup');
        assert.equal(d.receive(), false);
        d.classes.clear();
        d.classes.add('setup-active');
        assert.equal(d.receive(), false);
        d.classes.clear();
        d.controls.overlay = true;
        assert.equal(d.receive(), false);
        assert.equal(d.calls.length, 0, 'No navigation while initial UI/setup/unlock is unsettled');
    }
    {
        const d = dashboard();
        d.controls.locked = true;
        d.receive(); await flush();
        assert.equal(d.receive(), false);
        assert.equal(d.opens.length, 0, 'Native locked state must block even before overlay appears');
        await flush();
        d.controls.locked = false;
        d.controls.needsSetup = true;
        d.receive(); await flush();
        assert.equal(d.opens.length, 0);
        d.controls.needsSetup = false;
        d.controls.ready = false;
        d.receive(); await flush();
        assert.equal(d.opens.length, 0);
        d.controls.ready = true;
        assert.equal(d.receive(), false);
        await flush();
        assert.equal(d.receive(), true);
        assert.deepEqual(d.opens, ['abc'], 'Acknowledge once navigation has applied');
        assert.equal(d.receive(), true);
        assert.deepEqual(d.opens, ['abc'], 'Acknowledgement polling must not repeat navigation');
    }
    {
        const d = dashboard();
        const loading = deferred();
        d.context.window.channelsOpenNotificationRoute = () => loading.promise;
        const route = 'channels:' + 'ab'.repeat(16) + ':67656e6572616c';
        assert.equal(d.receive(route), false);
        await flush();
        assert.equal(d.receive(route), false, 'Async channels load is not an acknowledgement');
        loading.resolve(true); await flush();
        assert.equal(d.receive(route), true);
    }
    {
        const d = dashboard();
        const loading = deferred();
        let oldCurrent;
        d.context.window.openGameSession = (_id, current) => { oldCurrent = current; return loading.promise; };
        d.receive('lrgp:0123456789abcdef'); await flush();
        d.receive('lxmf:new', '2'); await flush();
        assert.equal(oldCurrent(), false);
        loading.resolve(true); await flush();
        assert.equal(d.receive('lxmf:new', '2'), true);
        assert.deepEqual(d.opens, ['new'], 'A stale async result must not acknowledge a newer route');
    }
    {
        const d = dashboard();
        d.context.window.openGameSession = () => undefined;
        d.receive('lrgp:0123456789abcdef'); await flush();
        assert.equal(d.receive('lrgp:0123456789abcdef'), false, 'Undefined is not successful game navigation');
    }
    {
        const d = dashboard();
        const startup = deferred();
        d.context.RS.invoke = () => startup.promise;
        d.receive();
        d.controls.now = 30001;
        startup.resolve({ stage: 'ready', needs_setup: false }); await flush();
        assert.deepEqual(d.opens, [], 'An expired native attempt cannot navigate later');
        assert.equal(d.receive('lxmf:abc', '1', 0), false);
        const reloaded = dashboard();
        assert.equal(reloaded.timers.length, 1, 'No perpetual JavaScript readiness polling');
        reloaded.timers[0]();
        assert.equal(reloaded.controls.wakes, 1, 'Same-WebView reload explicitly wakes native pending delivery');
        reloaded.receive('lxmf:abc', '2'); await flush();
        assert.equal(reloaded.receive('lxmf:abc', '2'), true);
    }
    {
        const loading = deferred();
        const selected = [];
        let current = true;
        const context = {
            window: {}, Promise, _allSessions: [],
            RS: { invoke: () => loading.promise },
            switchView: () => {}, renderSessionList: () => {},
            _findSession: id => context._allSessions.find(session => session.id === id),
            selectSession: id => selected.push(id)
        };
        vm.runInNewContext(gameCode, context);
        const result = context.window.openGameSession('game', () => current);
        current = false;
        loading.resolve([{ id: 'game' }]);
        assert.equal(await result, false);
        assert.deepEqual(selected, [], 'Superseded game loads must not select an old board');
        current = true;
        assert.equal(await context.window.openGameSession('game', () => current), true);
        assert.deepEqual(selected, ['game']);
        assert.equal(await context.window.openGameSession('missing'), false);
    }
    console.log('Android notification readiness, async routing, ownership and reload tests passed');
}

main().catch(error => { console.error(error); process.exitCode = 1; });
