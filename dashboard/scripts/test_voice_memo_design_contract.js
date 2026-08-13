#!/usr/bin/env node
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');

var root = path.join(__dirname, '..', '..');
function read(relative) {
    return fs.readFileSync(path.join(root, relative), 'utf8');
}

var html = read('dashboard/index.html');
var messaging = read('dashboard/static/css/09-messaging.css');
var responsive = read('dashboard/static/css/13-responsive.css');

var leading = html.indexOf('class="voice-memo-leading-slot"');
var signal = html.indexOf('class="voice-memo-signal"');
var timer = html.indexOf('id="voice-memo-timer"');
var trailing = html.indexOf('class="voice-memo-trailing-slot"');
var primary = html.indexOf('class="voice-memo-primary-slot"');

assert(leading !== -1 && signal !== -1 && timer !== -1 && trailing !== -1 && primary !== -1,
    'the recorder must expose stable leading, signal, timing, trailing, and primary slots');
assert(leading < signal && signal < timer && timer < trailing && trailing < primary,
    'recorder slots must retain the established left-to-right hierarchy');
assert(html.includes('class="loading-spinner voice-memo-state-spinner"'),
    'recorder transitions must reuse the canonical loading spinner');
assert(html.includes('id="voice-memo-inline-status"'),
    'recorder transitions need one concise inline status hook');
assert(html.includes('id="voice-memo-announcer" role="status" aria-live="polite"') &&
    html.includes('id="voice-memo-alert" role="alert" aria-live="assertive"'),
    'routine voice states and terminal interruptions need separate live-region priorities');
assert(!/id="voice-memo-timer"[^>]*aria-live/.test(html),
    'the recording timer must not announce every elapsed second');
assert(!html.includes('voice-memo-player-speed') && !html.includes('voice-memo-transcript'),
    'voice messages must stay a restrained recorder/player rather than a media console');

assert(/\.voice-memo-field\s*\{[\s\S]*?display:\s*grid;[\s\S]*?grid-template-columns:[^;]*minmax\(0, 1fr\)/.test(messaging),
    'the desktop recorder field must use stable grid tracks');
assert(/\.voice-memo-primary-slot\s*\{[\s\S]*?var\(--control-height-md\)/.test(messaging),
    'the desktop primary action must retain the shared 40px control geometry');
assert(/\.voice-memo-leading-slot,[\s\S]*?\.voice-memo-trailing-slot\s*\{[\s\S]*?var\(--space-16\)/.test(messaging),
    'inner desktop action slots must remain fixed while states change');

for (var recorderState of [
    'requesting_permission',
    'starting',
    'recording',
    'paused',
    'stopping',
    'review',
    'sending',
    'error',
]) {
    assert(messaging.includes('data-state="' + recorderState + '"'),
        'recorder CSS must define the ' + recorderState + ' state');
}

for (var playbackState of [
    'idle',
    'loading',
    'starting',
    'playing',
    'paused',
    'ended',
    'stalled',
    'recovering',
    'error',
]) {
    assert(messaging.includes('data-playback-state="' + playbackState + '"'),
        'player CSS must define the ' + playbackState + ' state');
}
assert(messaging.includes('.voice-memo-player-spinner') &&
    messaging.includes('.voice-memo-player-status'),
    'player state transitions need canonical spinner and concise status hooks');
assert(/prefers-reduced-motion:[^)]*reduce[\s\S]*?voice-memo/.test(messaging),
    'the signal-head treatment must remain still under reduced motion');

assert(/\.voice-memo-field\s*\{[\s\S]*?grid-template-columns:\s*var\(--touch-target\)/.test(responsive),
    'mobile recorder tracks must use the canonical touch target');
assert(/\.voice-memo-leading-slot,[\s\S]*?\.voice-memo-primary-slot\s*\{[\s\S]*?var\(--touch-target\)/.test(responsive),
    'every mobile recorder action slot must retain 44px geometry');
assert(/\.voice-memo-player-play\s*\{[\s\S]*?width:\s*44px;[\s\S]*?height:\s*44px/.test(responsive),
    'mobile voice playback must retain its existing 44px touch target');

console.log('Voice memo design contract tests passed');
