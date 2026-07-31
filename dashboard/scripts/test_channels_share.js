#!/usr/bin/env node
// Deterministic regressions for key-free channel share targets and explicit
// preview -> connect -> join transitions.

'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var channelsSource = fs.readFileSync(
    path.join(__dirname, '..', 'static', 'js', 'channels.js'),
    'utf8'
);
var contactCardSource = fs.readFileSync(
    path.join(__dirname, '..', 'static', 'js', 'contact_card.js'),
    'utf8'
);

function sourceRange(startName, endName) {
    var start = channelsSource.indexOf('function ' + startName);
    var end = channelsSource.indexOf('\nfunction ' + endName, start);
    assert(start !== -1 && end !== -1, startName + ' must exist');
    return channelsSource.slice(start, end);
}

function snapshot(generation, revision, phase, destination, selectedDestination, rooms) {
    return {
        protocol_version: '0.1.3',
        generation: generation,
        revision: revision,
        phase: phase,
        nickname: 'Field Rat',
        selected_hub_destination: selectedDestination === undefined
            ? destination
            : selectedDestination,
        hub: destination ? { destination_hash: destination } : null,
        hubs: [],
        rooms: rooms || [],
        directory: {
            phase: 'idle',
            rooms: [],
            complete: false,
            omitted_count: 0,
            refreshed_at_ms: null,
            last_error: null
        },
        history: {
            phase: 'ready',
            pending_events: 0,
            dropped_events: 0,
            last_error: null
        },
        hub_greeting: null,
        notices: [],
        last_error: null,
        updated_at_ms: revision
    };
}

function applyContext(initial) {
    var joined = [];
    var selected = [];
    var context = {
        channelsSnapshot: initial,
        channelsPendingShareJoin: null,
        channelsActiveRoom: null,
        channelsHistorySelection: null,
        channelsSavedRooms: [],
        _channelsSavedRoomsHub: null,
        _channelsSavedRoomKeys: {},
        _channelsDirectoryRequestSeq: 0,
        _channelsDirectoryRefreshPromise: null,
        _channelsLocalRoomEvents: {},
        _channelsExpandedPresenceGroups: {},
        _channelsSelectedMemberKey: null,
        _channelsMemberReturnFocusKey: null,
        _channelsRoomByName: function(roomName) {
            return context.channelsSnapshot.rooms.find(function(room) {
                return room.name === roomName;
            }) || null;
        },
        _channelsSelectedRoomView: function() { return null; },
        _channelsHistoryContext: function() { return null; },
        _channelsScheduleHistorySync: function() {},
        _channelsPersistConveniences: function() {},
        _channelsViewVisible: function() { return false; },
        _channelsDirectoryNeedsRefresh: function() { return false; },
        channelsRefreshDirectory: function() {},
        channelsLoadSavedRooms: function() {},
        channelsRefreshRoomIndex: function() {},
        renderChannels: function() {},
        channelHubRenderHome: function() {},
        channelsOpenJoinSheet: function(room) { joined.push(room); },
        channelsSelectRoom: function(room) { selected.push(room); },
        setTimeout: function(callback) { callback(); },
        Number: Number,
        String: String,
        Array: Array,
        Object: Object
    };
    vm.runInNewContext(
        sourceRange('_channelsSnapshotVersion', 'channelsLoad'),
        context,
        { filename: 'channels-share-snapshot.js' }
    );
    return { context: context, joined: joined, selected: selected };
}

function main() {
    var shareFlow = sourceRange('_channelsSafeShareFileName', 'channelsOpenConnectSheet');
    var connectSheet = sourceRange('channelsOpenConnectSheet', '_channelsSheetField');
    var hubOptions = sourceRange('channelsOpenHubOptions', 'channelsOpenRoomOptions');
    var roomOptions = sourceRange('channelsOpenRoomOptions', '_channelsRoomDetail');

    assert(shareFlow.indexOf("RS.invoke('api_channel_share'") !== -1,
        'share creation must cross the canonical Rust builder');
    assert(shareFlow.indexOf("previewCommand: 'api_preview_channel_share'") !== -1,
        'QR scans must cross the canonical Rust parser');
    assert(shareFlow.indexOf("RS.invoke('api_preview_channel_share'") !== -1,
        'pasted links must cross the canonical Rust parser');
    assert(shareFlow.indexOf('input.maxLength = 230;') !== -1,
        'paste input must match the QR-safe native ceiling');
    assert(shareFlow.indexOf('channelsOpenConnectSheet({') !== -1);
    assert(shareFlow.indexOf('channelsOpenJoinSheet(target.room)') !== -1);
    assert(shareFlow.indexOf("RS.invoke('connect_channel_hub'") === -1,
        'preview code must not connect as a side effect');
    assert(shareFlow.indexOf("RS.invoke('join_channel'") === -1,
        'preview code must not join as a side effect');
    assert(shareFlow.indexOf('localStorage') === -1,
        'imported targets and pending transitions must stay ephemeral');
    assert(shareFlow.indexOf('join_key') === -1 && shareFlow.indexOf('key:') === -1,
        'share generation and parsing must not accept a room key field');

    assert(connectSheet.indexOf("sharedRoom ? 'Connect and review' : 'Connect'") !== -1);
    assert(connectSheet.indexOf('preserve_pending_share: true') !== -1);
    assert(connectSheet.indexOf('channelsPendingShareJoin = pendingShare') !== -1);
    assert(hubOptions.indexOf("shareButton.textContent = 'Share hub'") !== -1);
    assert(roomOptions.indexOf("share.textContent = 'Share'") !== -1);
    assert(contactCardSource.indexOf('window.RS.qr = {') !== -1,
        'channel shares must reuse the common QR implementation');
    assert(contactCardSource.indexOf('openScanner: openContactQrScanner') !== -1);

    var exact = applyContext(snapshot(1, 1, 'connecting', 'hub-a'));
    exact.context.channelsPendingShareJoin = {
        destination_hash: 'hub-a',
        room: 'field',
        generation: 1
    };
    assert.strictEqual(
        exact.context.channelsApplySnapshot(snapshot(1, 2, 'active', 'hub-a')),
        true
    );
    assert.deepStrictEqual(exact.joined, ['field'],
        'only authenticated Active state on the exact hub opens join review');
    assert.strictEqual(exact.context.channelsPendingShareJoin, null);

    var wrongHub = applyContext(snapshot(1, 1, 'connecting', 'hub-a'));
    wrongHub.context.channelsPendingShareJoin = {
        destination_hash: 'hub-a',
        room: 'field',
        generation: 1
    };
    wrongHub.context.channelsApplySnapshot(snapshot(1, 2, 'active', 'hub-b'));
    assert.deepStrictEqual(wrongHub.joined, []);
    assert.strictEqual(wrongHub.context.channelsPendingShareJoin, null,
        'an active snapshot for another hub must cancel the pending transition');

    var switching = applyContext(snapshot(1, 1, 'connecting', 'hub-b', 'hub-a'));
    switching.context.channelsPendingShareJoin = {
        destination_hash: 'hub-a',
        room: 'field',
        generation: 1
    };
    switching.context.channelsApplySnapshot(
        snapshot(1, 2, 'active', 'hub-b', 'hub-a')
    );
    assert.deepStrictEqual(switching.joined, []);
    assert.notStrictEqual(switching.context.channelsPendingShareJoin, null,
        'the old live projection must not cancel an accepted hub switch');
    switching.context.channelsApplySnapshot(
        snapshot(1, 3, 'active', 'hub-a', 'hub-a')
    );
    assert.deepStrictEqual(switching.joined, ['field']);

    var autoRejoin = applyContext(snapshot(1, 1, 'connecting', 'hub-a'));
    autoRejoin.context.channelsPendingShareJoin = {
        destination_hash: 'hub-a',
        room: 'field',
        generation: 1
    };
    autoRejoin.context.channelsApplySnapshot(snapshot(
        1,
        2,
        'active',
        'hub-a',
        'hub-a',
        [{ name: 'field', phase: 'joining' }]
    ));
    assert.deepStrictEqual(autoRejoin.joined, []);
    assert.deepStrictEqual(autoRejoin.selected, []);
    assert.notStrictEqual(autoRejoin.context.channelsPendingShareJoin, null,
        'a durable auto-rejoin in flight must not open a duplicate join form');
    autoRejoin.context.channelsApplySnapshot(snapshot(
        1,
        3,
        'active',
        'hub-a',
        'hub-a',
        [{ name: 'field', phase: 'joined' }]
    ));
    assert.deepStrictEqual(autoRejoin.joined, []);
    assert.deepStrictEqual(autoRejoin.selected, ['field'],
        'an already-confirmed durable membership only needs navigation');
    assert.strictEqual(autoRejoin.context.channelsPendingShareJoin, null);

    var retired = applyContext(snapshot(1, 1, 'connecting', 'hub-a'));
    retired.context.channelsPendingShareJoin = {
        destination_hash: 'hub-a',
        room: 'field',
        generation: 1
    };
    retired.context.channelsApplySnapshot(snapshot(2, 1, 'active', 'hub-a'));
    assert.deepStrictEqual(retired.joined, []);
    assert.strictEqual(retired.context.channelsPendingShareJoin, null,
        'a replacement manager generation must retire imported intent');

    var failed = applyContext(snapshot(1, 1, 'connecting', 'hub-a'));
    failed.context.channelsPendingShareJoin = {
        destination_hash: 'hub-a',
        room: 'field',
        generation: 1
    };
    failed.context.channelsApplySnapshot(snapshot(1, 2, 'error', 'hub-a'));
    assert.deepStrictEqual(failed.joined, []);
    assert.strictEqual(failed.context.channelsPendingShareJoin, null,
        'a terminal connection error must retire imported intent');

    console.log('channel share tests passed');
}

main();
