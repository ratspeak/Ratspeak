#!/usr/bin/env node
// Deterministic regression for API responses racing live Channels snapshots.

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

function snapshot(generation, revision, roomPhase, sessionPhase) {
    return {
        protocol_version: '0.1.3',
        generation: generation,
        revision: revision,
        phase: sessionPhase || 'active',
        nickname: 'Field Rat',
        hub: null,
        rooms: roomPhase ? [{ name: 'general', phase: roomPhase, members: [], transcript: [] }] : [],
        hub_greeting: null,
        notices: [],
        last_error: null,
        updated_at_ms: revision
    };
}

async function main() {
    var savedHubs = deferred();
    var hubOverview = deferred();
    var staleJoining = snapshot(1, 2, 'joining');
    var context = {
        channelsSnapshot: snapshot(1, 1, null),
        channelsActiveRoom: null,
        channelsSavedHubs: [],
        channelsSavedRooms: [],
        _channelsLocalRoomEvents: {},
        _channelsExpandedPresenceGroups: {},
        _channelsSelectedMemberKey: null,
        _channelsMemberReturnFocusKey: null,
        _channelsSavedRoomsHub: null,
        _channelsSavedRoomKeys: {},
        _channelsLoadPromise: null,
        _channelsLoadedAt: 0,
        _channelsLastHubRefreshAt: 0,
        _channelsRoomByName: function() { return null; },
        _channelsPersistConveniences: function() {},
        renderChannels: function() {},
        channelHubRenderHome: function() {},
        channelsLoadSavedRooms: function() { return Promise.resolve([]); },
        channelsRefreshAvailableHubs: function() { return Promise.resolve([]); },
        _channelsIsConnected: function() {
            return context.channelsSnapshot.phase === 'active' ||
                context.channelsSnapshot.phase === 'stale';
        },
        channelHubLoad: function() { return hubOverview.promise; },
        RS: {
            invoke: function(command) {
                if (command === 'api_channels') return Promise.resolve(staleJoining);
                if (command === 'api_saved_channel_hubs') return savedHubs.promise;
                throw new Error('unexpected command: ' + command);
            }
        }
    };

    vm.runInNewContext(
        sourceRange('_channelsSnapshotVersion', 'channelsLoad') + '\n' +
            sourceRange('channelsLoad', 'channelsRefreshAvailableHubs'),
        context
    );

    // `api_channels` captures Joining, but Promise.all remains held by the
    // saved-hub and hosted-hub reads. A live event advances to Joined first.
    var load = context.channelsLoad(true);
    assert.strictEqual(context.channelsApplySnapshot(snapshot(1, 3, 'joined')), true);
    savedHubs.resolve([]);
    hubOverview.resolve(null);
    await load;
    assert.strictEqual(context.channelsSnapshot.rooms[0].phase, 'joined',
        'a delayed API batch must not overwrite a newer live event');
    assert.strictEqual(context.channelsSnapshot.revision, 3);

    // A command response can race the same way and must use the same guard.
    assert.strictEqual(context.channelsApplySnapshot(staleJoining), false);
    assert.strictEqual(context.channelsSnapshot.rooms[0].phase, 'joined');

    var equalButDifferent = snapshot(1, 3, 'joining');
    assert.strictEqual(context.channelsApplySnapshot(equalButDifferent), false,
        'equal versions are idempotent and cannot replace state');
    assert.strictEqual(context.channelsSnapshot.rooms[0].phase, 'joined');

    assert.strictEqual(context.channelsApplySnapshot(snapshot(0, 999, null, 'error')), false,
        'an older manager generation can never supersede the current one');
    assert.strictEqual(context.channelsApplySnapshot(snapshot(2, 1, null, 'offline')), true,
        'a newly created manager generation must supersede the retired manager');
    assert.strictEqual(context.channelsSnapshot.phase, 'offline');

    assert.strictEqual(context.channelsApplySnapshot({ phase: 'error' }), false,
        'unversioned snapshots are not valid state-bearing API responses');

    var joinSheet = sourceRange('channelsOpenJoinSheet', 'channelsOpenHubOptions');
    assert(joinSheet.indexOf("result.snapshot") !== -1,
        'direct joins must apply the command response through the freshness guard');
    assert(joinSheet.indexOf('channelsLoad(true)') === -1,
        'direct joins must not start the stale multi-query reload');

    var connect = sourceRange('channelsConnectToHub', '_channelsPhaseLabel');
    assert(connect.indexOf('channelsApplySnapshot(snapshot)') !== -1,
        'connect responses must use the freshness guard');

    var partAndDisconnect = sourceRange('_channelsPartRoom', '_channelsHandleComposerResult');
    assert(partAndDisconnect.indexOf('channelsApplySnapshot(result.snapshot)') !== -1,
        'direct part responses must use the freshness guard');
    assert(partAndDisconnect.indexOf('channelsApplySnapshot(snapshot)') !== -1,
        'disconnect responses must use the freshness guard');

    var composer = sourceRange('_channelsHandleComposerResult', 'channelsSendMessage');
    assert(composer.indexOf('channelsApplySnapshot(result.snapshot)') !== -1,
        'local join and part commands must use the freshness guard');
    assert(composer.indexOf('channelsLoad(true)') === -1,
        'local join and part commands must not start a stale multi-query reload');

    assert(channelsSource.indexOf("RS.listen('channels_snapshot', function(snapshot) {\n    channelsApplySnapshot(snapshot);") !== -1,
        'live events must use the same freshness guard as command and API responses');

    console.log('channel snapshot ordering tests passed');
}

main().catch(function(error) {
    console.error(error);
    process.exitCode = 1;
});
