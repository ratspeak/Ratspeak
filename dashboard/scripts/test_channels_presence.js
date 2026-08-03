#!/usr/bin/env node
// Deterministic tests for the Channels conversation/presence boundary.

'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var dashboardRoot = path.join(__dirname, '..');
var channelsSource = fs.readFileSync(
    path.join(dashboardRoot, 'static', 'js', 'channels.js'),
    'utf8'
);
var channelsCss = fs.readFileSync(
    path.join(dashboardRoot, 'static', 'css', '09-channels.css'),
    'utf8'
);
var indexSource = fs.readFileSync(path.join(dashboardRoot, 'index.html'), 'utf8');

function sourceRange(startName, endName) {
    var start = channelsSource.indexOf('function ' + startName);
    var end = channelsSource.indexOf('\nfunction ' + endName, start);
    assert(start !== -1 && end !== -1, startName + ' source must exist');
    return channelsSource.slice(start, end);
}

var constantsStart = channelsSource.indexOf('var CHANNEL_MESSAGE_GROUP_WINDOW_MS');
var constantsEnd = channelsSource.indexOf('\nvar CHANNEL_HISTORY_PAGE_SIZE', constantsStart);
assert(constantsStart !== -1 && constantsEnd !== -1,
    'message grouping window must be explicit');

var context = { window: {}, Number: Number, String: String, Array: Array, Date: Date };
vm.runInNewContext(
    channelsSource.slice(constantsStart, constantsEnd) + '\n' +
        sourceRange('_channelsActivityTime', '_channelsBuildDaySeparator') + '\n' +
        sourceRange('_channelsIsRemotePresenceItem', '_channelsOrderTimelineEntries') + '\n' +
        sourceRange('_channelsMessageAuthorKey', '_channelsBuildTranscriptItem') + '\n' +
        'window.group = _channelsGroupConsecutiveMessages;\n' +
        'window.remotePresence = _channelsIsRemotePresenceItem;',
    context,
    { filename: 'channels-conversation-model.js' }
);

function entry(id, time, options) {
    options = options || {};
    return {
        order: options.order || 0,
        hubNotice: !!options.hubNotice,
        item: {
            id: id,
            kind: options.kind || 'message',
            timestamp_ms: options.timestamp || time,
            recorded_at_ms: time,
            source_hash: options.source || null,
            nickname: options.nickname || null,
            ours: !!options.ours,
            mentioned: !!options.mentioned,
            text: options.text || id
        }
    };
}

var grouped = context.window.group([
    entry('hu', 1_000, { ours: true }),
    entry('hi', 2_000, { ours: true })
]);
assert.deepStrictEqual(
    Array.from(grouped, function(item) { return item.messageGroup; }),
    ['start', 'end'],
    'adjacent messages from the same author should form one visual turn'
);

grouped = context.window.group([
    entry('one', 1_000, { source: 'aa' }),
    entry('two', 2_000, { source: 'aa' }),
    entry('three', 3_000, { source: 'aa' })
]);
assert.deepStrictEqual(
    Array.from(grouped, function(item) { return item.messageGroup; }),
    ['start', 'middle', 'end'],
    'longer same-author turns should have stable start/middle/end positions'
);

grouped = context.window.group([
    entry('one', 1_000, { source: 'aa' }),
    entry('boundary', 1_500, { kind: 'system', ours: true }),
    entry('two', 2_000, { source: 'aa' })
]);
assert.deepStrictEqual(
    Array.from(grouped, function(item) { return item.messageGroup; }),
    ['single', 'single', 'single'],
    'any intervening event must break a visual message turn'
);

grouped = context.window.group([
    entry('one', 1_000, { source: 'aa' }),
    entry('late', 1_000 + (5 * 60 * 1_000) + 1, { source: 'aa' }),
    entry('other-author', 1_000 + (5 * 60 * 1_000) + 2, { source: 'bb' })
]);
assert.deepStrictEqual(
    Array.from(grouped, function(item) { return item.messageGroup; }),
    ['single', 'single', 'single'],
    'time and author boundaries must prevent over-grouping'
);

grouped = context.window.group([
    entry('plain', 1_000, { source: 'aa' }),
    entry('mention', 2_000, { source: 'aa', mentioned: true })
]);
assert.deepStrictEqual(
    Array.from(grouped, function(item) { return item.messageGroup; }),
    ['single', 'single'],
    'a mention boundary must retain its own author marker'
);

assert.strictEqual(context.window.remotePresence({ kind: 'join', ours: false }), true);
assert.strictEqual(context.window.remotePresence({ kind: 'part', ours: false }), true);
assert.strictEqual(context.window.remotePresence({ kind: 'present', ours: false }), true);
assert.strictEqual(context.window.remotePresence({ kind: 'join', ours: true }), false,
    'our own join remains a useful session boundary');
assert.strictEqual(context.window.remotePresence({ kind: 'message', ours: false }), false);

var timelineSource = sourceRange('_channelsTimelineEntries', '_channelsBuildHistoryRail');
assert(timelineSource.includes('if (_channelsIsRemotePresenceItem(item)) return;'),
    'routine remote membership must stay out of the conversation timeline');
assert(!channelsSource.includes('function _channelsBuildPresenceGroup') &&
        !channelsSource.includes('function _channelsReconcileRosterPresence'),
    'the transcript must not rebuild a second, synthetic presence feed');
assert(!channelsCss.includes('.channel-presence-group') &&
        !channelsCss.includes('.channel-presence-event'),
    'removed transcript presence UI must not leave dead styling');
assert(channelsSource.includes('function _channelsObserveRoomMembers') &&
        channelsSource.includes("item.kind !== 'part'") &&
        channelsSource.includes('_channelsMemberRosterModel(room)'),
    'authenticated PARTED and roster state must continue to drive the people pane');

assert(indexSource.includes('<button class="channel-members-info" id="channel-members-info"'),
    'partial-roster help belongs beside the People here label');
assert(indexSource.includes(
    'Seen here remembers recent activity and peers already known to this device.'
), 'the people-pane explanation must describe bounded and known-peer memory');
assert(!indexSource.includes('id="channel-members-note"'),
    'partial-roster help must not consume a separate member-pane row');
assert(channelsCss.includes('.channel-members-info::after') &&
        channelsCss.includes('.channel-members-info.open::after'),
    'the compact roster affordance must work for pointer and touch input');
var memberTooltipCss = channelsCss
    .split('.channel-members-info::after')[1]
    .split('}')[0];
assert(memberTooltipCss.includes('left: 50%') &&
        memberTooltipCss.includes('transform: translateX(-50%)') &&
        memberTooltipCss.includes('box-sizing: border-box'),
    'the roster tooltip must center its bounded box instead of clipping');

assert(channelsCss.includes('.channel-event.message-group-start') &&
        channelsCss.includes('.channel-event.message-group-middle') &&
        channelsCss.includes('.channel-event.message-group-end'),
    'message-turn grouping must have intentional visual states');
assert(channelsCss.includes('message-group-end:hover .channel-event-time'),
    'continuation timestamps must remain available on intent');

process.stdout.write('Channels conversation and presence tests passed.\n');
