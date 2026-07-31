#!/usr/bin/env node
// Deterministic regressions for privacy-safe Channels notification routing.

'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var dashboardRoot = path.join(__dirname, '..');
var eventSource = fs.readFileSync(
    path.join(dashboardRoot, 'static', 'js', 'tauri_events.js'),
    'utf8'
);
var channelsSource = fs.readFileSync(
    path.join(dashboardRoot, 'static', 'js', 'channels.js'),
    'utf8'
);

function sourceRange(source, startText, endText) {
    var start = source.indexOf(startText);
    var end = source.indexOf(endText, start);
    assert(start !== -1 && end !== -1, startText + ' must exist');
    return source.slice(start, end);
}

async function main() {
    var channelRoutes = [];
    var lxmfRoutes = [];
    var gameRoutes = [];
    var tapContext = {
        Uint8Array: Uint8Array,
        TextDecoder: TextDecoder,
        decodeURIComponent: decodeURIComponent,
        parseInt: parseInt,
        window: {
            channelsOpenNotificationRoute: function(hub, room) {
                channelRoutes.push([hub, room]);
            },
            openGameSession: function(id) { gameRoutes.push(id); }
        },
        openConversationWith: function(id) { lxmfRoutes.push(id); }
    };
    vm.runInNewContext(
        sourceRange(
            eventSource,
            'function _decodeChannelNotificationRoute',
            '\nfunction _initNotificationTapRouting'
        ),
        tapContext,
        { filename: 'channels-notification-tap.js' }
    );

    var hub = 'ab'.repeat(16);
    var room = 'field café';
    var roomHex = Buffer.from(room, 'utf8').toString('hex');
    var decoded = tapContext._decodeChannelNotificationRoute(
        'channels:' + hub + ':' + roomHex
    );
    assert.strictEqual(decoded.hub_destination_hash, hub);
    assert.strictEqual(decoded.room_name, room);
    assert.strictEqual(
        tapContext._decodeChannelNotificationRoute('channels:' + hub + ':ff'),
        null,
        'invalid UTF-8 must never reach navigation'
    );
    assert.strictEqual(
        tapContext._decodeChannelNotificationRoute('channels:' + hub.toUpperCase() + ':' + roomHex),
        null,
        'notification routes use one canonical lowercase hash form'
    );

    tapContext._routeNotificationTap({
        notification: { extra: { route: 'channels:' + hub + ':' + roomHex } }
    });
    assert.deepStrictEqual(Array.from(channelRoutes[0]), [hub, room]);
    tapContext._routeNotificationTap({ extra: { route: 'lxmf:abc123' } });
    tapContext._routeNotificationTap({ extra: { route: 'lrgp:game-7' } });
    assert.deepStrictEqual(lxmfRoutes, ['abc123']);
    assert.deepStrictEqual(gameRoutes, ['game-7']);

    tapContext._routeNotificationTap({
        extra: { route: 'channels:' + hub + ':0' }
    });
    assert.strictEqual(channelRoutes.length, 1,
        'malformed channel routes must fail closed instead of falling through');

    var selected = [];
    var routeContext = {
        channelsSnapshot: {
            hub: { destination_hash: hub },
            rooms: [{ name: 'general', phase: 'joined' }]
        },
        channelsLoad: function() { return Promise.resolve({}); },
        switchView: function(view) { selected.push(['view', view]); },
        _channelsUtf8Length: function(value) { return Buffer.byteLength(value, 'utf8'); },
        _channelsRoomByName: function(name) {
            return name === 'general' ? { name: 'general', phase: 'joined' } : null;
        },
        channelsSelectRoom: function(name) { selected.push(['live', name]); },
        channelsSelectHistoryRoom: function(routeHub, routeRoom) {
            selected.push(['history', routeHub, routeRoom]);
        },
        window: {},
        Promise: Promise
    };
    var notificationRouteSource = sourceRange(
        channelsSource,
        'function channelsOpenNotificationRoute',
        '\nfunction _onChannelDetailExit'
    );
    vm.runInNewContext(
        notificationRouteSource,
        routeContext,
        { filename: 'channels-notification-route.js' }
    );

    assert.strictEqual(
        await routeContext.channelsOpenNotificationRoute(hub, 'general'),
        true
    );
    assert.deepStrictEqual(Array.from(selected[1]), ['live', 'general'],
        'a joined room on the authenticated active hub opens live');

    selected.length = 0;
    routeContext.channelsSnapshot.hub.destination_hash = '22'.repeat(16);
    assert.strictEqual(
        await routeContext.channelsOpenNotificationRoute(hub, room),
        true
    );
    assert.deepStrictEqual(Array.from(selected[1]), ['history', hub, room],
        'an offline room opens only its bounded local timeline');
    assert.strictEqual(notificationRouteSource.indexOf('connect_channel_hub'), -1);
    assert.strictEqual(notificationRouteSource.indexOf('join_with_key'), -1,
        'a notification route must never reconnect or carry a room key');

    console.log('channel notification route tests passed');
}

main().catch(function(error) {
    console.error(error);
    process.exitCode = 1;
});
