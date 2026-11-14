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
var composerSource = read('dashboard/static/js/composer.js');

// The composer and the recorder are one pill: every action sits in the same
// square slot on one bottom rail, so attach, Record/Send, and the recorder's
// actions never shift as text grows or states change, including in iOS WKWebView.
assert(/\.cmp-slot \{[^}]*width: var\(--cmp-slot\);[^}]*height: var\(--cmp-slot\);/.test(messaging),
    'attachment, send, and voice actions share one slot geometry');
assert(/\.cmp \{\s*--cmp-slot: 40px;/.test(messaging) &&
    messaging.includes('@media (max-width: 768px), (pointer: coarse) { .cmp { --cmp-slot: 44px; } }'),
    'the slot is 40px on desktop and the 44px touch target on a phone or coarse pointer');
assert(/\.cmp-row \{[^}]*align-items: flex-end;/.test(messaging),
    'equal-sized composer actions must stay on one bottom rail as text grows');
var recorder = html.slice(html.indexOf('id="lxmf-voice-recorder"'), html.indexOf('id="lxmf-char-count"'));
['voice-memo-discard-btn', 'voice-memo-pause-btn', 'voice-memo-stop-btn', 'voice-memo-send-btn'].forEach(function(id) {
    var at = recorder.indexOf('id="' + id + '"');
    assert(at !== -1 && recorder.lastIndexOf('class="cmp-slot', at) > recorder.lastIndexOf('</span>', at),
        id + ' sits in a composer slot');
});
assert(html.indexOf('id="lxmf-voice-recorder"') > html.indexOf('id="lxmf-compose-bar"') &&
    html.indexOf('id="lxmf-voice-recorder"') < html.indexOf('id="lxmf-char-count"'),
    'the recorder is the same pill as the composer');
assert(composerSource.includes("class=\"ui-icon-btn cmp-record\"") && composerSource.includes("class=\"ui-send cmp-send\""),
    'the shared composer builds the same single Record/Send slot');

var leading = html.indexOf('class="voice-memo-leading-slot"');
var signal = html.indexOf('class="voice-memo-signal"');
var timer = html.indexOf('id="voice-memo-timer"');
var trailing = html.indexOf('voice-memo-trailing-slot"');
var primary = html.indexOf('voice-memo-primary-slot"');

assert(leading !== -1 && signal !== -1 && timer !== -1 && trailing !== -1 && primary !== -1,
    'the recorder must expose stable leading, signal, timing, trailing, and primary slots');
assert(leading < signal && signal < timer && timer < trailing && trailing < primary,
    'recorder slots must retain the established left-to-right hierarchy');
assert(html.includes('class="ui-spin voice-memo-state-spinner"'),
    'recorder transitions must reuse the kit spinner');
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
assert(/\.voice-memo-field \{[^}]*grid-template-columns: var\(--cmp-slot\)/.test(messaging) &&
    /\.voice-memo-leading-slot \{[^}]*width: var\(--cmp-slot\);[^}]*height: var\(--cmp-slot\);/.test(messaging),
    'the leading slot keeps the slot geometry while states change');
assert(html.includes('class="cmp-slot voice-memo-trailing-slot"') && html.includes('class="cmp-slot voice-memo-primary-slot"'),
    'the trailing and primary actions keep fixed slots while states change');

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
assert(messaging.includes('.voice-memo-waveform-layer') &&
    messaging.includes('.voice-memo-waveform-played') &&
    messaging.includes('clip-path: inset(0 var(--voice-playback-unplayed) 0 0)') &&
    /\.voice-memo-waveform-played > span \{ background: var\(--accent-ink\); \}/.test(messaging),
    'playback must reveal one stable accent waveform over the muted base');
assert(!messaging.includes('will-change: clip-path'),
    'the small progress clip must not retain a persistent compositor allocation');
assert(/prefers-reduced-motion:[^)]*reduce[\s\S]*?voice-memo/.test(messaging),
    'the signal-head treatment must remain still under reduced motion');
assert(/prefers-reduced-motion:[^)]*reduce[\s\S]*?\.voice-memo-waveform-layer[\s\S]*?transition:\s*none/.test(messaging),
    'elapsed waveform progress must not animate when reduced motion is requested');

assert(/\.voice-memo-player-play/.test(read('dashboard/static/js/voice_memos.js')) &&
    read('dashboard/static/js/voice_memos.js').includes('<button class="ui-btn ui-btn--icon voice-memo-player-play"'),
    'voice playback uses the kit icon button, which takes the 44px touch size on a phone');

console.log('Voice memo design contract tests passed');
