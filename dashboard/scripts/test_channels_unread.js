#!/usr/bin/env node
// Deterministic regressions for identity-scoped Channels unread/read state.

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
var navSource = fs.readFileSync(
    path.join(dashboardRoot, 'static', 'js', 'nav.js'),
    'utf8'
);

function sourceRange(source, startText, endText) {
    var start = source.indexOf(startText);
    var end = source.indexOf(endText, start);
    assert(start !== -1 && end !== -1, startText + ' must exist');
    return source.slice(start, end);
}

function deferred() {
    var resolve;
    var reject;
    var promise = new Promise(function(ok, fail) {
        resolve = ok;
        reject = fail;
    });
    return { promise: promise, resolve: resolve, reject: reject };
}

function tick() {
    return new Promise(function(resolve) { setImmediate(resolve); });
}

async function main() {
    var attention = [];
    var unreadContext = {
        channelsUnread: {
            rooms: [],
            unread_total: 0,
            mention_total: 0,
            attention_total: 0
        },
        _channelsUnreadRequestSeq: 0,
        _channelsHistoryEpoch: 0,
        _channelsHistoryKey: function(hub, room) {
            return String(hub || '').toLowerCase() + '\n' +
                String(room || '').toLowerCase();
        },
        _channelsEl: function() { return null; },
        _channelsRenderList: function() {},
        setMessageUnreadSource: function(source, count) {
            attention.push([source, count]);
        },
        RS: { invoke: function() { return Promise.resolve({}); } },
        Promise: Promise
    };
    vm.runInNewContext(
        sourceRange(
            channelsSource,
            'function _channelsUnreadCount',
            '\nfunction _channelsHubByDestination'
        ),
        unreadContext,
        { filename: 'channels-unread-state.js' }
    );

    unreadContext.channelsApplyUnread({
        rooms: [
            {
                hub_destination_hash: '11'.repeat(16),
                room_name: 'general',
                unread_count: 4,
                mention_count: 2,
                notification_level: 'mentions'
            },
            {
                hub_destination_hash: '22'.repeat(16),
                room_name: 'muted',
                unread_count: 8,
                mention_count: 1,
                notification_level: 'mute'
            }
        ],
        unread_total: 12,
        mention_total: 3,
        attention_total: 2
    });
    assert.strictEqual(unreadContext.channelsUnread.unread_total, 12);
    assert.strictEqual(unreadContext.channelsUnread.rooms[1].unread_count, 8,
        'muting suppresses attention, not durable unread truth');
    assert.deepStrictEqual(Array.from(attention.pop()), ['channels', 2]);

    var first = deferred();
    var second = deferred();
    var unreadResponses = [first.promise, second.promise];
    unreadContext.RS.invoke = function(command) {
        assert.strictEqual(command, 'api_channel_unread');
        return unreadResponses.shift();
    };
    var oldRequest = unreadContext.channelsRefreshUnread();
    var newRequest = unreadContext.channelsRefreshUnread();
    second.resolve({
        rooms: [],
        unread_total: 5,
        mention_total: 5,
        attention_total: 5
    });
    await newRequest;
    first.resolve({
        rooms: [],
        unread_total: 99,
        mention_total: 99,
        attention_total: 99
    });
    await oldRequest;
    assert.strictEqual(unreadContext.channelsUnread.attention_total, 5,
        'an older native response must not replace newer unread state');

    var marks = [];
    var readEntry = {
        loaded: true,
        loading: false,
        syncing: false,
        latest_sequence: '9007199254740993',
        marked_sequence: '0',
        marking: false,
        mark_requested: false
    };
    var readContext = {
        document: { visibilityState: 'visible' },
        currentView: 'channels',
        _channelsHistoryEpoch: 0,
        channelsSnapshot: { history: { phase: 'ready', pending_events: 0 } },
        _channelsCurrentHistoryKey: function() { return 'hub\ngeneral'; },
        _channelsCompact: function() { return false; },
        _channelsHistoryEntry: function() { return readEntry; },
        channelsRefreshUnread: function() {},
        channelsPrepareVisibleRead: function() {},
        window: { RS: { diag: function() {} } },
        RS: {
            invoke: function(command, payload) {
                marks.push([command, payload]);
                return Promise.resolve({});
            }
        },
        setTimeout: setTimeout,
        Promise: Promise
    };
    vm.runInNewContext(
        sourceRange(
            channelsSource,
            'function _channelsContextIsVisible',
            '\nfunction _channelsEnsureHistory'
        ),
        readContext,
        { filename: 'channels-visible-read.js' }
    );
    var roomContext = {
        key: 'hub\ngeneral',
        hub_destination_hash: '11'.repeat(16),
        room_name: 'general'
    };
    readContext._channelsMaybeMarkRoomRead(roomContext, readEntry);
    await tick();
    assert.strictEqual(marks.length, 1);
    assert.strictEqual(marks[0][0], 'mark_channel_room_read');
    assert.strictEqual(
        marks[0][1].args.through,
        '9007199254740993',
        'the exact opaque sequence must cross IPC as a string'
    );

    readEntry.latest_sequence = '9007199254740994';
    readContext.document.visibilityState = 'hidden';
    readContext._channelsMaybeMarkRoomRead(roomContext, readEntry);
    assert.strictEqual(marks.length, 1,
        'background rooms must never be marked read');

    readContext.document.visibilityState = 'visible';
    readContext.channelsSnapshot.history.pending_events = 1;
    readContext._channelsMaybeMarkRoomRead(roomContext, readEntry);
    assert.strictEqual(marks.length, 1,
        'accepted events must drain to SQLite before advancing the cursor');

    var elements = {
        'nav-unread-dot': { style: {} },
        'nav-channels-unread': { style: {} },
        'bb-unread': { style: {} }
    };
    var badges = [
        {
            dataset: { messageModeBadge: 'direct' },
            hidden: true,
            setAttribute: function(name, value) { this[name] = value; }
        },
        {
            dataset: { messageModeBadge: 'channels' },
            hidden: true,
            setAttribute: function(name, value) { this[name] = value; }
        }
    ];
    var navContext = {
        _messageUnreadSources: { direct: 0, channels: 0 },
        document: {
            getElementById: function(id) { return elements[id] || null; },
            querySelectorAll: function() { return badges; }
        },
        window: {}
    };
    vm.runInNewContext(
        sourceRange(
            navSource,
            'function setMessageUnreadSource',
            '\nvar VIEWS'
        ),
        navContext,
        { filename: 'message-unread-reducer.js' }
    );
    navContext.setMessageUnreadSource('direct', 3);
    navContext.setMessageUnreadSource('channels', 2);
    navContext.setMessageUnreadSource('direct', 0);
    assert.strictEqual(elements['nav-unread-dot'].style.display, 'none');
    assert.strictEqual(elements['nav-channels-unread'].style.display, '');
    assert.strictEqual(elements['bb-unread'].style.display, '',
        'the shared mobile Messages badge must retain Channels attention');
    assert.strictEqual(badges[0].hidden, true);
    assert.strictEqual(badges[1].textContent, '2');

    console.log('channel unread and read-state tests passed');
}

main().catch(function(error) {
    console.error(error);
    process.exitCode = 1;
});
