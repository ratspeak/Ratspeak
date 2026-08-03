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
var nativeShareSource = fs.readFileSync(
    path.join(__dirname, '..', 'static', 'js', 'native_channel_share.js'),
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
        _channelsLiveItemSeenAt: {},
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
        _channelsObserveLiveItems: function() {},
        _channelsBeginMemberContinuity: function() {},
        _channelsObserveRoomMembers: function() {},
        _channelsResetMemberObservations: function() {},
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
    assert(shareFlow.indexOf('key:') === -1,
        'share generation and parsing must not construct a room key field');
    assert(
        shareFlow.indexOf("hasOwnProperty.call(target, 'key')") !== -1 &&
        shareFlow.indexOf("hasOwnProperty.call(target, 'join_key')") !== -1,
        'the native typed boundary must explicitly reject any injected key field'
    );

    assert(connectSheet.indexOf("sharedRoom ? 'Connect and review' : 'Connect'") !== -1);
    assert(connectSheet.indexOf('preserve_pending_share: true') !== -1);
    assert(connectSheet.indexOf('channelsPendingShareJoin = pendingShare') !== -1);
    assert(hubOptions.indexOf("shareButton.textContent = 'Share hub'") !== -1);
    assert(roomOptions.indexOf("share.textContent = 'Share'") !== -1);
    assert(contactCardSource.indexOf('window.RS.qr = {') !== -1,
        'channel shares must reuse the common QR implementation');
    assert(contactCardSource.indexOf('openScanner: openContactQrScanner') !== -1);
    assert(nativeShareSource.indexOf("RS.invoke('take_native_channel_share')") !== -1,
        'native URLs must drain only the app-owned typed inbox');
    assert(nativeShareSource.indexOf("'native_channel_share_available'") !== -1);
    assert(nativeShareSource.indexOf('_isSetupActive()') !== -1,
        'native previews must wait until first-run setup is complete');
    assert(nativeShareSource.indexOf('.bottom-sheet.open') !== -1,
        'native previews must not stack over an existing decision sheet');
    assert(nativeShareSource.indexOf('.modal-overlay.active') !== -1 &&
        nativeShareSource.indexOf('.game-modal-overlay') !== -1 &&
        nativeShareSource.indexOf('.block-list-overlay') !== -1 &&
        nativeShareSource.indexOf('#rs-image-viewer.open') !== -1 &&
        nativeShareSource.indexOf('.action-popover.open') !== -1,
        'native previews must wait for non-sheet decision surfaces too');
    assert(nativeShareSource.indexOf('deep-link://new-url') === -1,
        'the frontend must not subscribe to the plugin URL event');
    assert(nativeShareSource.indexOf('localStorage') === -1,
        'native targets must remain process-memory only');
    assert(nativeShareSource.indexOf('connect_channel_hub') === -1);
    assert(nativeShareSource.indexOf('join_channel') === -1);

    var nativeEntry = sourceRange(
        'channelsOpenNativeSharedChannel',
        'channelsScanSharedChannel'
    );
    var presentedNativeTargets = [];
    var nativeEntryContext = {
        window: {},
        Object: Object,
        String: String,
        _channelsPresentSharedTarget: function(target) {
            presentedNativeTargets.push(target);
        }
    };
    vm.runInNewContext(nativeEntry, nativeEntryContext, {
        filename: 'channels-native-entry.js'
    });
    var validNativeTarget = {
        format: 'ratspeak.channel.v1',
        payload: 'ratspeak://channel?v=1&hub=00112233445566778899aabbccddeeff',
        hub_destination_hash: '00112233445566778899aabbccddeeff',
        room: null
    };
    assert.strictEqual(
        nativeEntryContext.channelsOpenNativeSharedChannel(validNativeTarget),
        true
    );
    assert.deepStrictEqual(presentedNativeTargets, [validNativeTarget]);
    assert.strictEqual(
        nativeEntryContext.channelsOpenNativeSharedChannel(Object.assign(
            {},
            validNativeTarget,
            { key: 'must-never-cross-this-boundary' }
        )),
        false
    );
    assert.strictEqual(presentedNativeTargets.length, 1);

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
