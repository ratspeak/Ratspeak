#!/usr/bin/env node
'use strict';
// Production theme controller with a deterministic native-window model. The
// model follows vendored Tao: an explicit preference suppresses OS changes;
// clearing it restores OS tracking and only a colour change emits media events.
// This is regression coverage, not a claim of executing Windows/WebView2.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const source = fs.readFileSync(process.env.RATSPEAK_THEME_SOURCE ||
    path.join(__dirname, '../../dashboard/static/js/theme.js'), 'utf8');

function boot(options = {}) {
    const attrs = {}, storage = {...options.storage}, listeners = [], dom = [], calls = [], pending = [], bars = [];
    let systemDark = !!options.dark, forced = null, effectiveDark = systemDark;
    const syncMedia = () => {
        const next = forced === null ? systemDark : forced === 'dark';
        if (next === effectiveDark) return; // no event for clearing a same-colour override
        effectiveDark = next;
        if (!options.coalescedMedia) listeners.forEach(listener => listener({matches: next}));
    };
    const invoke = (name, args) => {
        assert.equal(name, 'set_native_theme');
        calls.push(args.theme);
        return new Promise((resolve, reject) => pending.push({theme: args.theme, resolve, reject}));
    };
    const window = {
        __RATSPEAK_DESKTOP__: options.desktop !== false,
        __RATSPEAK_NATIVE_THEME_AUTO__: options.nativeAuto !== false,
        RS: options.lateBridge ? {} : {invoke},
        matchMedia: () => ({get matches() { return effectiveDark; }, addEventListener(type, listener) {
            assert.equal(type, 'change'); listeners.push(listener);
        }}),
        dispatchEvent() {},
        CustomEvent: function(type, options) { this.type = type; this.detail = options.detail; }
    };
    if (options.android) window.RatspeakAndroid = {setColorMode: mode => bars.push(mode)};
    const document = {
        documentElement: {setAttribute: (key, value) => { attrs[key] = value; }, getAttribute: key => attrs[key]},
        querySelector: () => ({setAttribute() {}}),
        addEventListener: (type, listener) => { if (type === 'DOMContentLoaded') dom.push(listener); }
    };
    vm.runInNewContext(source, {window, document, CustomEvent: window.CustomEvent, Promise,
        localStorage: {getItem: key => storage[key] || null, setItem: (key, value) => {storage[key] = value;}, removeItem: key => {delete storage[key];}}});
    return {
        attrs, storage, calls, pending, bars,
        appearance: window.RS.appearance,
        ready() { window.RS.invoke = invoke; dom.forEach(listener => listener()); },
        os(dark) { systemDark = dark; syncMedia(); },
        finish(fail = false) {
            assert(pending.length, 'expected one pending native request');
            const request = pending.shift();
            if (fail) request.reject(new Error('synthetic native failure'));
            else { forced = request.theme === 'auto' ? null : request.theme; syncMedia(); request.resolve({theme: request.theme}); }
        },
        native: () => forced
    };
}
async function settle() { for (let i = 0; i < 10; i++) await Promise.resolve(); }
async function drain(app) {
    for (let i = 0; i < 8; i++) {
        await settle();
        if (!app.pending.length) return;
        assert.equal(app.pending.length, 1, 'at most one native setter may be in flight');
        app.finish();
    }
    throw Error('native theme feedback loop did not settle');
}

(async () => {
    for (const dark of [false, true]) {
        const app = boot({dark});
        assert.equal(app.calls[0], 'auto', 'System must be passed to native as a preference, not its current colour');
        app.ready(); await drain(app);
        assert.deepEqual(app.calls, ['auto'], 'System must clear native override, never pin the resolved colour');
        assert.equal(app.native(), null);
        app.os(!dark); await drain(app);
        assert.equal(app.attrs['data-theme'], dark ? 'light' : 'dark');
        assert.equal(app.attrs['data-theme-preference'], 'auto');
        assert.deepEqual(app.calls, ['auto'], 'OS events must not write another override');
        for (const forced of ['light', 'dark']) {
            app.appearance.commit('nord', forced); await drain(app);
            assert.equal(app.native(), forced);
            app.os(!dark); app.os(dark); await drain(app);
            assert.equal(app.attrs['data-theme'], forced, 'explicit choices ignore system colour');
            app.appearance.commit('nord', 'auto'); await drain(app);
            assert.equal(app.native(), null);
            assert.equal(app.attrs['data-theme'], dark ? 'dark' : 'light');
            app.os(!dark); await drain(app);
            assert.equal(app.attrs['data-theme'], dark ? 'light' : 'dark', 'System resumes without restart');
            assert.equal(app.attrs['data-theme-family'], 'nord');
            assert.equal(app.storage['rs-theme'], undefined);
            app.os(dark); await drain(app);
        }
        const beforeFamily = app.calls.length;
        app.appearance.commit('rose-pine', 'auto'); await drain(app);
        assert.equal(app.calls.length, beforeFamily, 'palette choice must not lock system appearance');
    }
    const rapid = boot(); await drain(rapid);
    rapid.appearance.commit('ratspeak', 'dark');
    rapid.appearance.commit('ratspeak', 'light');
    rapid.appearance.commit('ratspeak', 'auto');
    assert.deepEqual(rapid.calls, ['auto', 'dark']);
    await drain(rapid);
    assert.deepEqual(rapid.calls, ['auto', 'dark', 'auto'], 'coalesce to latest preference behind one pending call');
    assert.equal(rapid.native(), null);
    assert.equal(rapid.attrs['data-theme'], 'light');
    rapid.os(true); await drain(rapid); assert.equal(rapid.attrs['data-theme'], 'dark');

    const failed = boot(); failed.finish(true); await settle();
    assert.equal(failed.calls.length, 1, 'no unbounded retry on native failure');
    failed.appearance.commit('ratspeak', 'auto'); await drain(failed);
    assert.deepEqual(failed.calls, ['auto', 'auto'], 'failed state may retry on the next explicit sync');
    failed.appearance.commit('ratspeak', 'light');
    failed.appearance.commit('ratspeak', 'auto');
    failed.finish(true); await drain(failed);
    assert.equal(failed.native(), null, 'older failure must not discard a newer desired preference');

    const rollback = boot({coalescedMedia: true}); await drain(rollback);
    rollback.appearance.commit('nord', 'dark'); await drain(rollback);
    rollback.appearance.commit('nord', 'auto');
    assert.equal(rollback.attrs['data-theme'], 'dark', 'media still reflects the old override before IPC');
    await drain(rollback);
    assert.equal(rollback.attrs['data-theme'], 'light', 'native completion reconciles System even without a media event');
    assert.equal(rollback.attrs['data-theme-family'], 'nord');

    const late = boot({dark: true, lateBridge: true});
    assert.equal(late.calls.length, 0);
    late.ready(); await drain(late); assert.deepEqual(late.calls, ['auto']);
    const restart = boot({dark: false, storage: {'rs-theme': 'dark'}}); await drain(restart);
    assert.equal(restart.native(), 'dark');
    restart.appearance.commit('ratspeak', 'auto'); await drain(restart);
    assert.equal(restart.attrs['data-theme'], 'light');

    const browser = boot({desktop: false}); browser.os(true); browser.ready(); await drain(browser);
    assert.equal(browser.attrs['data-theme'], 'dark'); assert.equal(browser.calls.length, 0);
    const android = boot({desktop: false, android: true}); android.ready(); android.os(true);
    assert.deepEqual(android.bars, ['light', 'dark'], 'Android system bars require resolved colours, not native overrides');
    const gtk = boot({dark: true, nativeAuto: false}); await drain(gtk);
    assert.deepEqual(gtk.calls, ['dark'], 'retain GTK resolved-colour path: None means light in pinned Tao');
    console.log('Native theme regression: System/explicit transitions, OS changes, startup, serialization, failure, browser/Android/GTK compatibility passed');
})().catch(error => {console.error(error); process.exitCode = 1;});
