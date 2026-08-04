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
var storage = {
    'rs-theme': 'sepia',
    'rs-theme-family': 'unknown'
};
var attrs = {};
var events = [];
var mediaListeners = [];
var media = { matches: true };
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

var appearance = context.window.RS.appearance;
assert.strictEqual(
    JSON.stringify(appearance.families.map(function(family) { return family.id; })),
    JSON.stringify(['ratspeak', 'nord', 'solarized', 'gruvbox', 'catppuccin'])
);
assert.strictEqual(storage['rs-theme'], undefined, 'invalid legacy mode cache must be removed');
assert.strictEqual(storage['rs-theme-family'], undefined, 'invalid family cache must be removed');
assert.strictEqual(attrs['data-theme-family'], 'ratspeak');
assert.strictEqual(attrs['data-theme-preference'], 'auto');
assert.strictEqual(attrs['data-theme'], 'dark');
assert.strictEqual(meta.content, '#18171A');

appearance.commit('nord', 'light');
assert.strictEqual(storage['rs-theme-family'], 'nord');
assert.strictEqual(storage['rs-theme'], 'light');
assert.strictEqual(attrs['data-theme-family'], 'nord');
assert.strictEqual(attrs['data-theme'], 'light');
assert.strictEqual(meta.content, '#ECEFF4');
assert.strictEqual(events[events.length - 1].type, 'ratspeak-theme-changed');
assert.strictEqual(events[events.length - 1].detail.family, 'nord');
assert.strictEqual(events[events.length - 1].detail.preference, 'light');

appearance.commit('catppuccin', 'dark');
assert.strictEqual(meta.content, '#11111B');
assert.strictEqual(events[events.length - 1].detail.mode, 'dark');

appearance.commit('ratspeak', 'auto');
assert.strictEqual(storage['rs-theme-family'], undefined, 'default family should not need a cache entry');
assert.strictEqual(storage['rs-theme'], undefined, 'system mode should preserve legacy missing-key semantics');
media.matches = false;
mediaListeners.forEach(function(listener) { listener({ matches: false }); });
assert.strictEqual(attrs['data-theme'], 'light', 'system-mode changes must resolve immediately');
assert.strictEqual(events[events.length - 1].detail.preference, 'auto');

console.log('Appearance theme tests passed');
