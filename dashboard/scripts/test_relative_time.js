#!/usr/bin/env node
// Sanity test for RS.relativeTime / RS.relativeTimeFrom in dashboard/static/js/state.js.
// Plain node, no framework: extracts the formatter source and evaluates it
// against a stubbed window. Run: node dashboard/scripts/test_relative_time.js

'use strict';

var fs = require('fs');
var path = require('path');

var src = fs.readFileSync(path.join(__dirname, '..', 'static', 'js', 'state.js'), 'utf8');

var start = src.indexOf('window.RS.relativeTimeFrom');
if (start === -1) throw new Error('relativeTimeFrom not found in state.js');
var wrapperStart = src.indexOf('window.RS.relativeTime =', start);
if (wrapperStart === -1) throw new Error('relativeTime wrapper not found in state.js');
var end = src.indexOf('\n};', wrapperStart);
if (end === -1) throw new Error('end of relativeTime wrapper not found');
var snippet = src.slice(start, end + 3);

var window = { RS: {} };
new Function('window', snippet)(window);
var RS = window.RS;

var NOW = 1750000000; // arbitrary fixed epoch seconds
var failures = 0;

function check(label, actual, expected) {
    if (actual === expected) {
        console.log('  ok  ' + label + ' -> ' + JSON.stringify(actual));
    } else {
        failures++;
        console.log('FAIL  ' + label + ' -> ' + JSON.stringify(actual) + ' (expected ' + JSON.stringify(expected) + ')');
    }
}

function ago(diffSec) { return RS.relativeTimeFrom(NOW, NOW - diffSec); }

// Falsy / missing timestamps (health.js BLE rows rely on '').
check('then=0 (missing)', RS.relativeTimeFrom(NOW, 0), '');
check('then=NaN (undefined ms / 1000)', RS.relativeTimeFrom(NOW, Math.floor(undefined / 1000)), '');

// Boundaries: just now / s / m / h / d.
check('diff 0s', ago(0), 'just now');
check('diff 4s', ago(4), 'just now');
check('diff 5s', ago(5), '5s ago');
check('diff 59s', ago(59), '59s ago');
check('diff 60s', ago(60), '1m ago');
check('diff 3599s', ago(3599), '59m ago');
check('diff 3600s', ago(3600), '1h ago');
check('diff 86399s', ago(86399), '23h ago');
check('diff 86400s (day case health.js lacked)', ago(86400), '1d ago');
check('diff 14d (no week unit; stays days)', ago(14 * 86400), '14d ago');

// Future timestamp (clock skew) collapses to 'just now', as both originals did.
check('diff -30s (future)', ago(-30), 'just now');

// health.js call-site conversion: epoch ms -> Math.floor(ms / 1000).
var ms = (NOW - 90) * 1000 + 999;
check('ms call-site conversion (90s ago)', RS.relativeTimeFrom(NOW, Math.floor(ms / 1000)), '1m ago');

// Wall-clock wrapper sanity.
check('RS.relativeTime(now-120s)', RS.relativeTime(Date.now() / 1000 - 120), '2m ago');

if (failures > 0) {
    console.log(failures + ' failure(s)');
    process.exit(1);
}
console.log('all checks passed');
