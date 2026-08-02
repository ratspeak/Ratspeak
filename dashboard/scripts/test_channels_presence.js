#!/usr/bin/env node
// Deterministic tests for Channels presence cleanup. Plain Node, no DOM needed.

'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var channelsPath = path.join(__dirname, '..', 'static', 'js', 'channels.js');
var channelsSource = fs.readFileSync(channelsPath, 'utf8');
var constantsStart = channelsSource.indexOf('var CHANNEL_PRESENCE_REJOIN_WINDOW_MS');
var constantsEnd = channelsSource.indexOf('\n\nfunction _channelsEl', constantsStart);
var activityStart = channelsSource.indexOf('function _channelsActivityTime');
var activityEnd = channelsSource.indexOf('\nfunction _channelsBuildDaySeparator', activityStart);
var functionsStart = channelsSource.indexOf('function _channelsIsPresenceEvent');
var functionsEnd = channelsSource.indexOf('\nfunction _channelsAppendPresenceCount', functionsStart);

assert(constantsStart !== -1 && constantsEnd !== -1, 'presence constants must exist');
assert(activityStart !== -1 && activityEnd !== -1, 'activity clock helpers must exist');
assert(functionsStart !== -1 && functionsEnd !== -1, 'presence helpers must exist');

var context = { window: {}, Number: Number, String: String, Array: Array };
var exportsSource = '\nwindow.ChannelsPresence = {' +
    'collapse: _channelsCollapseTransientRejoins,' +
    'group: _channelsGroupPresenceEvents,' +
    'order: _channelsOrderTimelineEntries,' +
    'rows: _channelsPresenceGroupRows,' +
    'summary: _channelsPresenceGroupSummary,' +
    'tooltip: _channelsPresenceTooltip' +
    '};';
vm.runInNewContext(
    channelsSource.slice(constantsStart, constantsEnd) +
        '\n' +
        channelsSource.slice(activityStart, activityEnd) +
        '\n' +
        channelsSource.slice(functionsStart, functionsEnd) +
        exportsSource,
    context,
    { filename: 'channels-presence.js' }
);

var presence = context.window.ChannelsPresence;
var rosterStart = channelsSource.indexOf('function _channelsRosterMemberKey');
var rosterEnd = channelsSource.indexOf('\nfunction _channelsSavedHub', rosterStart);
assert(rosterStart !== -1 && rosterEnd !== -1, 'roster reconciliation helpers must exist');
var rosterEvents = [];
var rosterContext = {
    window: {},
    _channelsRosterBaselines: {},
    channelsSnapshot: { rooms: [] },
    _channelsHistoryKey: function(hub, room) { return hub + '|' + room; },
    _channelsMemberName: function(member) {
        return member.nickname || member.identity_hash || 'Channel member';
    },
    _channelsPresenceIdentityKey: function(item) {
        if (item.source_hash) return 'source:' + String(item.source_hash).toLowerCase();
        return item.nickname ? 'nickname:' + String(item.nickname).toLowerCase() : '';
    },
    _channelsAddLocalRoomItem: function(room, item) {
        rosterEvents.push({ room: room, item: item });
    }
};
vm.runInNewContext(
    channelsSource.slice(rosterStart, rosterEnd) +
        '\nwindow.reconcile = _channelsReconcileRosterPresence;',
    rosterContext,
    { filename: 'channels-roster-presence.js' }
);
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
            recorded_at_ms: options.recordedAt || timestamp,
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

test('a quick rejoin still cancels when other membership activity is interleaved', function() {
    var entries = [
        event('part', 1000, 'aabbcc', 'Ada'),
        event('join', 4000, 'ddeeff', 'Grace'),
        event('join', 9000, 'aabbcc', 'Ada')
    ];
    var result = presence.collapse(entries);
    assert.strictEqual(result.length, 1);
    assert.strictEqual(result[0].item.nickname, 'Grace');
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

test('membership activity stays grouped until a message even when events are minutes apart', function() {
    var entries = [
        event('join', 1_000, null, 'DhC'),
        event('part', 23_000, null, 'DhC'),
        event('join', 387_000, null, 'Brongus')
    ];
    var result = presence.group(entries, 'lobby');
    assert.strictEqual(result.length, 1);
    assert.ok(result[0].presenceGroup);
    assert.strictEqual(result[0].presenceGroup.entries.length, 3);

    var summary = presence.summary(result[0].presenceGroup);
    assert.strictEqual(summary.joined.length, 2);
    assert.strictEqual(summary.left.length, 1);
    assert.strictEqual(summary.text, '2 people joined and 1 left');
});

test('members discovered in the entry roster are described as here, not newly joined', function() {
    var entries = [
        event('present', 1000, 'present-1', 'v6z'),
        event('present', 2000, 'present-2', 'Ada')
    ];
    var result = presence.group(entries, 'general');
    assert.strictEqual(result.length, 1);
    assert.ok(result[0].presenceGroup);
    var summary = presence.summary(result[0].presenceGroup);
    assert.strictEqual(summary.present.length, 2);
    assert.strictEqual(summary.text, '2 people here');
});

test('entry roster context does not merge with later join activity', function() {
    var entries = [
        event('present', 1000, 'present-1', 'v6z'),
        event('join', 2000, 'join-1', 'Ada')
    ];
    var result = presence.group(entries, 'general');
    assert.strictEqual(result.length, 2);
    assert.strictEqual(presence.summary(result[0].presenceGroup || {
        entries: [result[0]]
    }).text, '1 person here');
    assert.strictEqual(result[1].item.kind, 'join');
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

test('local leave activity is ordered before a message observed later', function() {
    var entries = [
        event('join', 1000, 'join-1', 'Ada'),
        event('join', 2000, 'join-2', 'Grace'),
        event('message', 5000, 'speaker', 'Linus'),
        event('part', 3000, 'part-1', 'Margaret')
    ];
    var ordered = presence.order(entries);
    assert.deepStrictEqual(
        Array.from(ordered, function(entry) { return entry.item.kind; }),
        ['join', 'join', 'part', 'message']
    );
    var rendered = presence.group(ordered, 'general');
    assert.strictEqual(rendered.length, 2);
    assert.strictEqual(
        presence.summary(rendered[0].presenceGroup).text,
        '2 people joined and 1 left'
    );
    assert.strictEqual(rendered[1].item.kind, 'message');
});

test('reconnect churn counts unique people and expands to one row per action', function() {
    var entries = [
        event('join', 1000, 'aabbcc', 'Ada'),
        event('join', 2000, 'aabbcc', 'Ada'),
        event('join', 3000, 'aabbcc', 'Ada'),
        event('join', 4000, 'ddeeff', 'Grace'),
        event('part', 5000, 'aabbcc', 'Ada')
    ];
    var result = presence.group(entries, 'general');
    var summary = presence.summary(result[0].presenceGroup);
    assert.strictEqual(summary.text, '2 people joined and 1 left');
    assert.strictEqual(summary.rows.length, 3);
    assert.strictEqual(summary.joined[0].occurrences, 3);
    assert.strictEqual(summary.joined[0].item.timestamp_ms, 3000,
        'the compact row keeps the most recent occurrence for its timestamp');
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

test('roster reconciliation supplies honest context and only fills missing deltas', function() {
    function member(identity, nickname, isSelf) {
        return { identity_hash: identity, nickname: nickname, is_self: !!isSelf };
    }
    var bob = member('self', 'Bob', true);
    var v6z = member('v6z-id', 'v6z', false);
    var ada = member('ada-id', 'Ada', false);
    var grace = member('grace-id', 'Grace', false);
    var linus = member('linus-id', 'Linus', false);
    var room = {
        name: 'lobby',
        phase: 'joined',
        members: [bob, v6z],
        transcript: []
    };
    rosterContext.channelsSnapshot.rooms = [room];
    rosterContext.window.reconcile('hub');
    assert.strictEqual(rosterEvents.length, 1);
    assert.strictEqual(rosterEvents[0].item.kind, 'present');
    assert.strictEqual(rosterEvents[0].item.text, 'v6z is here');

    room.members = [bob, v6z, ada];
    room.transcript = [{
        id: 'joined-ada',
        kind: 'join',
        source_hash: 'hub-id',
        nickname: 'Ada'
    }];
    rosterContext.window.reconcile('hub');
    assert.strictEqual(rosterEvents.length, 1,
        'a native join event must not be duplicated by roster inference');

    room.members = [bob, v6z, ada, grace];
    rosterContext.window.reconcile('hub');
    assert.strictEqual(rosterEvents[1].item.text, 'Grace joined');

    room.members = [bob, v6z, ada, grace, linus];
    room.transcript.push({
        id: 'message-linus',
        kind: 'message',
        source_hash: 'linus-id',
        nickname: 'Linus'
    });
    rosterContext.window.reconcile('hub');
    assert.strictEqual(rosterEvents.length, 2,
        'a member first observed through a message must not get a fabricated join');

    room.members = [bob, v6z, ada, linus];
    rosterContext.window.reconcile('hub');
    assert.strictEqual(rosterEvents[2].item.text, 'Grace left');
});

tests.forEach(function(entry) {
    entry.fn();
    process.stdout.write('\u2713 ' + entry.name + '\n');
});

assert(channelsSource.indexOf('function _channelsIsConnectionLifecycleItem') !== -1,
    'legacy reconnect markers must be recognized outside the human timeline');
assert(channelsSource.indexOf("item.text === 'Reconnected to hub'") !== -1,
    'legacy reconnect copy must remain presentation-filtered');

process.stdout.write('\n' + tests.length + ' Channels presence tests passed.\n');
