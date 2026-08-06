#!/usr/bin/env node
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var source = fs.readFileSync(
    path.join(__dirname, '..', 'static', 'js', 'theme.js'),
    'utf8'
);

function bootTheme(storage, prefersDark) {
    var attrs = {};
    var events = [];
    var mediaListeners = [];
    var media = { matches: !!prefersDark };
    var meta = {
        content: '',
        setAttribute: function(name, value) {
            if (name === 'content') this.content = value;
        }
    };
    var root = {
        setAttribute: function(name, value) { attrs[name] = value; },
        getAttribute: function(name) { return attrs[name] || null; }
    };
    function CustomEvent(name, options) {
        this.type = name;
        this.detail = options.detail;
    }
    var context = {
        CustomEvent: CustomEvent,
        localStorage: {
            getItem: function(key) {
                return Object.prototype.hasOwnProperty.call(storage, key) ? storage[key] : null;
            },
            setItem: function(key, value) { storage[key] = String(value); },
            removeItem: function(key) { delete storage[key]; }
        },
        document: {
            documentElement: root,
            querySelector: function(selector) {
                return selector === 'meta[name="theme-color"]' ? meta : null;
            },
            addEventListener: function() {}
        },
        window: {
            CustomEvent: CustomEvent,
            dispatchEvent: function(event) { events.push(event); },
            matchMedia: function() {
                return {
                    get matches() { return media.matches; },
                    addEventListener: function(type, listener) {
                        if (type === 'change') mediaListeners.push(listener);
                    }
                };
            }
        }
    };
    context.window.window = context.window;
    context.window.document = context.document;
    context.window.localStorage = context.localStorage;
    vm.createContext(context);
    vm.runInContext(source, context, { filename: 'theme.js' });
    return {
        appearance: context.window.RS.appearance,
        attrs: attrs,
        events: events,
        media: media,
        mediaListeners: mediaListeners,
        meta: meta,
        storage: storage
    };
}

var storage = {
    'rs-theme': 'sepia',
    'rs-theme-family': 'unknown'
};
var firstBoot = bootTheme(storage, true);
var appearance = firstBoot.appearance;

assert.strictEqual(
    JSON.stringify(appearance.families.map(function(family) { return family.id; })),
    JSON.stringify(['ratspeak', 'nord', 'everforest', 'gruvbox', 'catppuccin', 'rose-pine'])
);
assert.strictEqual(storage['rs-theme'], undefined, 'invalid legacy mode cache must be removed');
assert.strictEqual(storage['rs-theme-family'], undefined, 'invalid family cache must be removed');
assert.strictEqual(firstBoot.attrs['data-theme-family'], 'ratspeak');
assert.strictEqual(firstBoot.attrs['data-theme-preference'], 'auto');
assert.strictEqual(firstBoot.attrs['data-theme'], 'dark');
assert.strictEqual(firstBoot.meta.content, '#18171A');

appearance.commit('nord', 'light');
assert.strictEqual(storage['rs-theme-family'], 'nord');
assert.strictEqual(storage['rs-theme'], 'light');
assert.strictEqual(firstBoot.attrs['data-theme-family'], 'nord');
assert.strictEqual(firstBoot.attrs['data-theme'], 'light');
assert.strictEqual(firstBoot.meta.content, '#ECEFF4');
assert.strictEqual(firstBoot.events[firstBoot.events.length - 1].type, 'ratspeak-theme-changed');
assert.strictEqual(firstBoot.events[firstBoot.events.length - 1].detail.family, 'nord');
assert.strictEqual(firstBoot.events[firstBoot.events.length - 1].detail.preference, 'light');

appearance.commit('everforest', 'dark');
assert.strictEqual(storage['rs-theme-family'], 'everforest');
assert.strictEqual(storage['rs-theme'], 'dark');
assert.strictEqual(firstBoot.meta.content, '#232A2E');

appearance.commit('rose-pine', 'dark');
assert.strictEqual(storage['rs-theme-family'], 'rose-pine');
assert.strictEqual(firstBoot.attrs['data-theme-family'], 'rose-pine');
assert.strictEqual(firstBoot.meta.content, '#232136');
var roseRestart = bootTheme(storage, false);
assert.strictEqual(roseRestart.attrs['data-theme-family'], 'rose-pine');
assert.strictEqual(roseRestart.attrs['data-theme'], 'dark');
assert.strictEqual(roseRestart.meta.content, '#232136');

appearance.commit('everforest', 'dark');

// A fresh JS realm models a fully closed and relaunched app. The pre-paint
// cache must restore both axes before settings hydration can reach SQLite.
var coldRestart = bootTheme(storage, false);
assert.strictEqual(coldRestart.attrs['data-theme-family'], 'everforest');
assert.strictEqual(coldRestart.attrs['data-theme-preference'], 'dark');
assert.strictEqual(coldRestart.attrs['data-theme'], 'dark');
assert.strictEqual(coldRestart.meta.content, '#232A2E');

coldRestart.appearance.commit('everforest', 'auto');
assert.strictEqual(storage['rs-theme-family'], 'everforest');
assert.strictEqual(storage['rs-theme'], undefined, 'system mode should preserve missing-key semantics');
var autoRestart = bootTheme(storage, false);
assert.strictEqual(autoRestart.attrs['data-theme-family'], 'everforest');
assert.strictEqual(autoRestart.attrs['data-theme-preference'], 'auto');
assert.strictEqual(autoRestart.attrs['data-theme'], 'light');
assert.strictEqual(autoRestart.meta.content, '#EFEBD4');
autoRestart.media.matches = true;
autoRestart.mediaListeners.forEach(function(listener) { listener({ matches: true }); });
assert.strictEqual(autoRestart.attrs['data-theme'], 'dark', 'system-mode changes must resolve immediately');
assert.strictEqual(autoRestart.meta.content, '#232A2E');

var legacyStorage = {
    'rs-theme-family': 'solarized',
    'rs-theme': 'light'
};
var migratedBoot = bootTheme(legacyStorage, false);
assert.strictEqual(migratedBoot.attrs['data-theme-family'], 'everforest');
assert.strictEqual(legacyStorage['rs-theme-family'], 'everforest',
    'the retired Solarized cache must migrate without losing the selected family');

autoRestart.appearance.commit('ratspeak', 'auto');
assert.strictEqual(storage['rs-theme-family'], undefined, 'default family should not need a cache entry');
assert.strictEqual(storage['rs-theme'], undefined, 'default mode should not need a cache entry');

console.log('Appearance theme tests passed');
