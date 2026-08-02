#!/usr/bin/env node
// Deterministic regressions for identity-scoped Channels history pagination.

'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var channelsPath = path.join(__dirname, '..', 'static', 'js', 'channels.js');
var channelsSource = fs.readFileSync(channelsPath, 'utf8');

function sourceRange(startName, endName) {
    var start = channelsSource.indexOf('function ' + startName);
    var end = channelsSource.indexOf('\nfunction ' + endName, start);
    assert(start !== -1 && end !== -1, startName + ' must exist');
    return channelsSource.slice(start, end);
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

function historyItem(id, sequence, recordedAt, timestamp) {
    return {
        event_id: id,
        sequence: sequence,
        kind: 'message',
        timestamp_ms: timestamp || recordedAt,
        recorded_at_ms: recordedAt,
        source_hash: 'peer-' + id,
        nickname: id,
        text: id,
        ours: false
    };
}

async function main() {
    var calls = [];
    var pages = [];
    var renders = [];
    var transcript = { scrollHeight: 800, scrollTop: 120, clientHeight: 300 };
    var context = {
        CHANNEL_HISTORY_PAGE_SIZE: 100,
        CHANNEL_HISTORY_SYNC_PAGE_SIZE: 200,
        CHANNEL_HISTORY_CACHE_ROOM_LIMIT: 5000,
        CHANNEL_HISTORY_MAX_SYNC_PAGES: 32,
        _channelsHistoryCache: {},
        _channelsHistoryRequestSeq: 0,
        _channelsHistoryEpoch: 0,
        document: { visibilityState: 'hidden' },
        _channelsEl: function() { return transcript; },
        _channelsCurrentHistoryKey: function() { return 'hub-a\ngeneral'; },
        _channelsRenderRoom: function(restore) { renders.push(restore); },
        channelsSnapshot: {
            history: { phase: 'unavailable', pending_events: 0 }
        },
        window: { RS: { diag: function() {} } },
        RS: {
            invoke: function(command, payload) {
                assert.strictEqual(command, 'api_channel_history');
                calls.push(payload.args);
                return Promise.resolve(pages.shift());
            }
        }
    };
    vm.runInNewContext(
        sourceRange('_channelsHistoryEntry', 'channelsRefreshSavedHubs'),
        context,
        { filename: 'channels-history-loader.js' }
    );
    context._channelsCurrentHistoryKey = function() { return 'hub-a\ngeneral'; };

    var room = {
        key: 'hub-a\ngeneral',
        hub_destination_hash: 'hub-a',
        room_name: 'general'
    };
    pages.push({
        items: [
            historyItem('three', '3', 3000),
            historyItem('four', '4', 4000)
        ],
        next_before: '9007199254740993',
        next_after: '4',
        has_more: true
    });
    var entry = await context._channelsLoadHistory(room, false);
    assert.deepStrictEqual(
        Array.from(entry.items, function(item) { return item.id; }),
        ['three', 'four']
    );
    assert.strictEqual(calls[0].before, null);
    assert.strictEqual(entry.next_before, '9007199254740993',
        'the opaque 64-bit cursor must remain a string');

    pages.push({
        items: [
            historyItem('one', '1', 1000),
            historyItem('two', '2', 2000),
            historyItem('three', '3', 3000)
        ],
        next_before: null,
        has_more: false
    });
    entry = await context._channelsLoadHistory(room, true);
    assert.strictEqual(calls[1].before, '9007199254740993',
        'pagination must send the exact exclusive cursor returned by native code');
    assert.deepStrictEqual(
        Array.from(entry.items, function(item) { return item.id; }),
        ['one', 'two', 'three', 'four'],
        'older pages prepend chronologically and duplicate event IDs collapse'
    );
    assert.strictEqual(renders[1].key, room.key);
    assert.strictEqual(renders[1].scroll_height, 800,
        'prepending captures a scroll anchor before replacing the DOM');

    pages.push({
        items: [historyItem('five', '5', 5000)],
        next_before: null,
        next_after: '5',
        has_more: true
    });
    pages.push({
        items: [historyItem('six', '6', 6000)],
        next_before: null,
        next_after: '6',
        has_more: false
    });
    await context._channelsSyncHistory(room);
    assert.strictEqual(calls[2].after, '4');
    assert.strictEqual(calls[3].after, '5');
    assert.deepStrictEqual(
        Array.from(entry.items, function(item) { return item.id; }),
        ['one', 'two', 'three', 'four', 'five', 'six'],
        'forward catch-up closes the gap beyond the bounded live snapshot'
    );

    var pending = deferred();
    context.RS.invoke = function() { return pending.promise; };
    var other = {
        key: 'hub-b\nfield',
        hub_destination_hash: 'hub-b',
        room_name: 'field'
    };
    var staleLoad = context._channelsLoadHistory(other, false);
    context._channelsHistoryEpoch++;
    pending.resolve({
        items: [historyItem('wrong-identity', '5', 5000)],
        next_before: null,
        has_more: false
    });
    await staleLoad;
    assert.strictEqual(context._channelsHistoryCache[other.key].items.length, 0,
        'a response from the previous identity epoch must be discarded');

    var timelineContext = {
        channelsSnapshot: {
            notices: [],
            history: { phase: 'ready' }
        },
        _channelsLiveItemSeenAt: { 'general\npending': 12_000 },
        _channelsLocalRoomEvents: { general: [] },
        _channelsLiveItemKey: function(room, id) { return room + '\n' + id; },
        _channelsIsHubNotice: function() { return false; },
        _channelsIsConnectionLifecycleItem: function(item) {
            return !!item && item.kind === 'system' &&
                item.text === 'Reconnected to hub';
        }
    };
    vm.runInNewContext(
        sourceRange('_channelsTimelineEntries', '_channelsBuildHistoryRail') +
            '\n' + sourceRange('_channelsOrderTimelineEntries',
                '_channelsPresenceIdentityKey'),
        timelineContext,
        { filename: 'channels-history-merge.js' }
    );
    var storedCurrent = context._channelsNormalizeHistoryItem(
        historyItem('current', '11', 11_000, 999_999)
    );
    var merged = timelineContext._channelsTimelineEntries({
        name: 'general',
        phase: 'joined',
        transcript: [
            {
                id: 'current',
                kind: 'message',
                timestamp_ms: 1,
                source_hash: 'peer-current',
                nickname: 'Current',
                text: 'current',
                ours: false
            },
            {
                id: 'pending',
                kind: 'message',
                timestamp_ms: 2,
                source_hash: 'peer-pending',
                nickname: 'Pending',
                text: 'pending',
                ours: false
            },
            {
                id: 'legacy-reconnect',
                kind: 'system',
                timestamp_ms: 3,
                source_hash: null,
                nickname: null,
                text: 'Reconnected to hub',
                ours: true
            }
        ]
    }, {
        items: [
            context._channelsNormalizeHistoryItem(historyItem('older', '10', 10_000)),
            storedCurrent
        ]
    });
    assert.deepStrictEqual(
        Array.from(merged, function(entryValue) { return entryValue.item.id; }),
        ['older', 'current', 'pending'],
        'receive sequence, not peer timestamps, orders human activity and hides legacy Link lifecycle rows'
    );
    assert.strictEqual(merged[1].item.recorded_at_ms, 11_000,
        'a live overlap inherits its trusted local receive time');

    assert(channelsSource.indexOf('localStorage.setItem') === -1,
        'channel history must never be copied into browser storage');
    assert(channelsSource.indexOf("RS.invoke('api_channel_room_index')") !== -1,
        'the offline room browser must use the bookmark/history union');
    assert(channelsSource.indexOf('latest_recorded_at_ms') !== -1,
        'retained history must stay discoverable and sort by local receive time');
    assert(channelsSource.indexOf('Local timeline') === -1,
        'a healthy local history store must not occupy a persistent transcript rail');
    assert(channelsSource.indexOf('api_saved_channel_room_index') === -1,
        'a bookmark-only index would hide history after forgetting a hub');
    console.log('channel history tests passed');
}

main().catch(function(error) {
    console.error(error);
    process.exitCode = 1;
});
