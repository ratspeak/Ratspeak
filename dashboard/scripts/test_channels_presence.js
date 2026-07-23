#!/usr/bin/env node
// Deterministic tests for Channels presence cleanup. Plain Node, no DOM needed.

'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var channelsPath = path.join(__dirname, '..', 'static', 'js', 'channels.js');
var channelsSource = fs.readFileSync(channelsPath, 'utf8');
var constantsStart = channelsSource.indexOf('var CHANNEL_PRESENCE_GROUP_WINDOW_MS');
var constantsEnd = channelsSource.indexOf('\n\nfunction _channelsEl', constantsStart);
var functionsStart = channelsSource.indexOf('function _channelsIsPresenceEvent');
var functionsEnd = channelsSource.indexOf('\nfunction _channelsAppendPresenceCount', functionsStart);

assert(constantsStart !== -1 && constantsEnd !== -1, 'presence constants must exist');
assert(functionsStart !== -1 && functionsEnd !== -1, 'presence helpers must exist');

var context = { window: {}, Number: Number, String: String, Array: Array };
var exportsSource = '\nwindow.ChannelsPresence = {' +
    'collapse: _channelsCollapseTransientRejoins,' +
    'group: _channelsGroupPresenceEvents,' +
    'summary: _channelsPresenceGroupSummary,' +
    'tooltip: _channelsPresenceTooltip' +
    '};';
vm.runInNewContext(
    channelsSource.slice(constantsStart, constantsEnd) +
        '\n' +
        channelsSource.slice(functionsStart, functionsEnd) +
        exportsSource,
    context,
    { filename: 'channels-presence.js' }
);

var presence = context.window.ChannelsPresence;
var tests = [];

function test(name, fn) {
    tests.push({ name: name, fn: fn });
}

function event(kind, timestamp, sourceHash, nickname, options) {
    options = options || {};
    return {
        order: options.order || 0,
        hubNotice: !!options.hubNotice,
        item: {
            id: options.id || kind + '-' + timestamp + '-' + (sourceHash || nickname || 'member'),
            kind: kind,
            timestamp_ms: timestamp,
            source_hash: sourceHash || null,
            nickname: nickname || null,
            ours: !!options.ours,
            text: options.text || ''
        }
    };
}

test('a quick adjacent leave and rejoin from the same identity cancel out', function() {
    var entries = [
        event('part', 1000, 'AABBCC', 'Ada'),
        event('join', 9000, 'aabbcc', 'Ada')
    ];
    assert.strictEqual(presence.collapse(entries).length, 0);
    assert.strictEqual(presence.group(entries, 'general').length, 0);
});

test('an intervening channel event preserves both presence events', function() {
    var entries = [
        event('part', 1000, 'aabbcc', 'Ada'),
        event('message', 4000, 'ddeeff', 'Grace'),
        event('join', 9000, 'aabbcc', 'Ada')
    ];
    var result = presence.collapse(entries);
    assert.strictEqual(result.length, 3);
    assert.strictEqual(result[0].item.kind, 'part');
    assert.strictEqual(result[1].item.kind, 'message');
    assert.strictEqual(result[2].item.kind, 'join');
});

test('a rejoin after the transient window remains visible', function() {
    var entries = [
        event('part', 1000, 'aabbcc', 'Ada'),
        event('join', 17000, 'aabbcc', 'Ada')
    ];
    assert.strictEqual(presence.collapse(entries).length, 2);
});

test('different identities with the same nickname never cancel out', function() {
    var entries = [
        event('part', 1000, 'aabbcc', 'Ada'),
        event('join', 9000, 'ddeeff', 'Ada')
    ];
    assert.strictEqual(presence.collapse(entries).length, 2);
});

test('nickname matching is used only when neither event has an identity hash', function() {
    var nicknameOnly = [
        event('part', 1000, null, 'Ada'),
        event('join', 9000, null, 'ada')
    ];
    var mixedIdentity = [
        event('part', 1000, 'aabbcc', 'Ada'),
        event('join', 9000, null, 'Ada')
    ];
    assert.strictEqual(presence.collapse(nicknameOnly).length, 0);
    assert.strictEqual(presence.collapse(mixedIdentity).length, 2);
});

test('our own join confirmation remains visible', function() {
    var entries = [
        event('part', 1000, 'aabbcc', 'Identity 3', { ours: true }),
        event('join', 9000, 'aabbcc', 'Identity 3', { ours: true })
    ];
    assert.strictEqual(presence.collapse(entries).length, 2);
});

test('existing consecutive presence grouping still applies after cleanup', function() {
    var entries = [
        event('part', 1000, 'aabbcc', 'Ada'),
        event('part', 2000, 'ddeeff', 'Grace'),
        event('join', 8000, 'ddeeff', 'Grace')
    ];
    var result = presence.group(entries, 'general');
    assert.strictEqual(result.length, 1);
    assert.strictEqual(result[0].item.source_hash, 'aabbcc');
});

test('mixed uninterrupted presence activity becomes one truthful summary', function() {
    var entries = [
        event('part', 1000, 'part-1', 'Ada'),
        event('part', 2000, 'part-2', 'Grace'),
        event('join', 3000, 'join-1', 'Linus'),
        event('join', 4000, 'join-2', 'Margaret'),
        event('join', 5000, 'join-3', 'Edsger'),
        event('part', 6000, 'part-3', 'Ken'),
        event('join', 7000, 'join-4', 'Barbara'),
        event('join', 8000, 'join-5', 'Donald'),
        event('join', 9000, 'join-6', 'Radia')
    ];
    var result = presence.group(entries, 'general');
    assert.strictEqual(result.length, 1);
    assert.ok(result[0].presenceGroup);
    assert.strictEqual(result[0].presenceGroup.entries.length, 9);

    var summary = presence.summary(result[0].presenceGroup);
    assert.strictEqual(summary.joined.length, 6);
    assert.strictEqual(summary.left.length, 3);
    assert.strictEqual(summary.text, '6 people joined and 3 left');
});

test('a message ends a mixed presence group', function() {
    var entries = [
        event('join', 1000, 'join-1', 'Ada'),
        event('part', 2000, 'part-1', 'Grace'),
        event('message', 3000, 'speaker', 'Linus'),
        event('join', 4000, 'join-2', 'Margaret'),
        event('part', 5000, 'part-2', 'Edsger')
    ];
    var result = presence.group(entries, 'general');
    assert.strictEqual(result.length, 3);
    assert.ok(result[0].presenceGroup);
    assert.strictEqual(result[1].item.kind, 'message');
    assert.ok(result[2].presenceGroup);
});

test('count hover text prefers names and retains a full hash fallback', function() {
    var entries = [
        event('join', 1000, 'aabbcc', 'Ada'),
        event('join', 2000, '0123456789abcdef0123456789abcdef', null)
    ];
    assert.strictEqual(
        presence.tooltip(entries),
        'Ada, 0123456789abcdef0123456789abcdef'
    );
});

tests.forEach(function(entry) {
    entry.fn();
    process.stdout.write('\u2713 ' + entry.name + '\n');
});

process.stdout.write('\n' + tests.length + ' Channels presence tests passed.\n');
