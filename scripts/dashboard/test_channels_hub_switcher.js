#!/usr/bin/env node
// Deterministic model and source coverage for the one-live-hub switcher.

'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var root = path.join(__dirname, '..', '..', 'dashboard');
var channelsSource = fs.readFileSync(
    path.join(root, 'static', 'js', 'channels.js'),
    'utf8'
);
var channelHubSource = fs.readFileSync(
    path.join(root, 'static', 'js', 'channel_hub.js'),
    'utf8'
);
var indexSource = fs.readFileSync(path.join(root, 'index.html'), 'utf8');
var cssSource = fs.readFileSync(
    path.join(root, 'static', 'css', '09-channels.css'),
    'utf8'
);

function sourceRange(startName, endName) {
    var start = channelsSource.indexOf('function ' + startName + '(');
    var end = channelsSource.indexOf('\nfunction ' + endName, start);
    assert(start !== -1 && end !== -1, startName + ' source range must exist');
    return channelsSource.slice(start, end) + '\n';
}

var modelSource = sourceRange('_channelsMergedHubs', '_channelsSetText');
var context = {
    Object: Object,
    String: String,
    channelsSavedHubs: [],
    channelsDiscoveredHubs: [],
    channelsSnapshot: {},
    channelHubOwnDestinationHash: function() { return ''; }
};
context._channelsIsConnected = function() {
    return context.channelsSnapshot.phase === 'active' ||
        context.channelsSnapshot.phase === 'stale';
};
vm.runInNewContext(modelSource, context, {
    filename: 'channels-hub-switcher-model.js'
});

var HUB_A = '00112233445566778899aabbccddeeff';
var HUB_B = '11112222333344445555666677778888';
var HUB_C = '22223333444455556666777788889999';
var HUB_D = '3333444455556666777788889999aaaa';

context.channelsSavedHubs = [
    {
        destination_hash: HUB_A,
        label: 'Home relay',
        nickname: 'rat',
        last_connected: 30
    },
    {
        destination_hash: HUB_B,
        label: 'Field relay',
        nickname: 'rat',
        last_connected: 20
    },
    {
        destination_hash: HUB_C,
        label: 'Archive relay',
        nickname: 'rat',
        last_connected: 10
    }
];
context.channelsDiscoveredHubs = [
    {
        destination_hash: HUB_A,
        announced_name: 'Home',
        hops: 0,
        last_seen: 100
    },
    {
        destination_hash: HUB_B,
        announced_name: 'Field',
        hops: 1,
        last_seen: 90
    },
    {
        destination_hash: HUB_D,
        announced_name: 'Ridge',
        hops: 2,
        last_seen: 80
    }
];
context.channelsSnapshot = {
    phase: 'active',
    selected_hub_destination: HUB_A,
    hub: {
        destination_hash: HUB_A,
        name: 'Home',
        hops: 0
    }
};

var model = context._channelsHubSwitcherModel();
assert.strictEqual(model.current.destination_hash, HUB_A);
assert.strictEqual(model.current.saved, true);
assert.strictEqual(model.current.nearby, true);
assert.strictEqual(
    Array.prototype.map.call(model.nearby, function(hub) {
        return hub.destination_hash;
    }).join(','),
    HUB_B + ',' + HUB_D,
    'recently heard choices must preserve hop-first ordering'
);
assert.strictEqual(model.saved.length, 1);
assert.strictEqual(model.saved[0].destination_hash, HUB_C);
assert(
    !Array.prototype.some.call(model.nearby, function(hub) {
        return hub.destination_hash === HUB_A;
    }),
    'the selected hub must be pinned once, not repeated as another choice'
);

context.channelsSavedHubs = [];
context.channelsDiscoveredHubs = [];
context.channelsSnapshot = {
    phase: 'connecting',
    selected_hub_destination: HUB_D,
    hub: {
        destination_hash: HUB_D,
        announced_name: 'Uncached target'
    }
};
model = context._channelsHubSwitcherModel();
assert.strictEqual(model.current.destination_hash, HUB_D,
    'an unsaved in-flight target must remain visible as the current selection');
assert.strictEqual(model.current.announced_name, 'Uncached target');

function mode(phase, target, selected) {
    context.channelsSnapshot = {
        phase: phase,
        selected_hub_destination: selected || null,
        hub: selected ? { destination_hash: selected } : null
    };
    return context._channelsHubConnectMode(target).kind;
}

assert.strictEqual(mode('active', HUB_A, HUB_A), 'current');
assert.strictEqual(mode('stale', HUB_B, HUB_A), 'switch');
assert.strictEqual(mode('reconnecting', HUB_A, HUB_A), 'recovering');
assert.strictEqual(mode('reconnecting', HUB_B, HUB_A), 'switch',
    'a person may deliberately replace a bounded recovery attempt');
assert.strictEqual(mode('resolving', HUB_B, HUB_A), 'pending');
assert.strictEqual(mode('connecting', HUB_B, HUB_A), 'pending');
assert.strictEqual(mode('awaiting_welcome', HUB_B, HUB_A), 'pending');
assert.strictEqual(mode('error', HUB_A, HUB_A), 'connect',
    'an ended attempt remains retryable');
assert.strictEqual(mode('offline', HUB_B, null), 'connect');

// ---------------------------------------------------------------- hub strip

var stripContext = {
    Object: Object,
    String: String,
    channelsSavedHubs: [],
    channelsDiscoveredHubs: [],
    channelsSnapshot: {},
    channelHubOwnDestinationHash: function() { return ''; },
    _channelsHubName: function(hub) { return hub.name || hub.announced_name || hub.label || 'Channel hub'; },
    _channelsDefaultNickname: function() { return 'rat'; },
    _channelsOwnedHubReady: function() { return false; },
    channelsPendingHubLabel: '',
    _anyInterfaceOnline: true
};
stripContext._channelsIsConnecting = function() {
    return ['resolving', 'connecting', 'awaiting_welcome', 'reconnecting']
        .indexOf(stripContext.channelsSnapshot.phase) !== -1;
};
stripContext._channelsNetworkOnline = function() { return stripContext._anyInterfaceOnline; };
stripContext._channelsFirstViewState = function() {
    return stripContext._anyInterfaceOnline === false ? 'offline' : 'listening';
};
vm.runInNewContext(
    sourceRange('_channelsMergedHubs', '_channelsHubSwitcherModel') +
        sourceRange('_channelsSelectedHubDestination', '_channelsCurrentHubObserved') +
        sourceRange('_channelsCurrentHub', '_channelsFirstView') +
        sourceRange('_channelsHubStripModel', '_channelsRenderHubStrip'),
    stripContext,
    { filename: 'channels-hub-strip.js' }
);

function strip(phase, hub, extra) {
    stripContext.channelsSnapshot = Object.assign({
        phase: phase,
        nickname: 'Bob',
        selected_hub_destination: hub ? hub.destination_hash : null,
        hub: hub || null,
        last_error: null
    }, extra || {});
    var model = stripContext._channelsHubStripModel();
    return [model.title, model.sub, model.dot + (model.pulse ? '+pulse' : ''), model.menu ? 'menu' : 'status'].join(' | ');
}

var LIVE = { destination_hash: HUB_A, name: 'MichMesh.hub' };
assert.strictEqual(strip('active', LIVE), 'MichMesh.hub | Connected as Bob | online | menu');
assert.strictEqual(strip('stale', LIVE), 'MichMesh.hub | Reconnecting… | warning+pulse | menu',
    'a recovering link keeps the hub and says it is reconnecting');
assert.strictEqual(strip('reconnecting', LIVE), 'MichMesh.hub | Reconnecting… | warning+pulse | menu');
assert.strictEqual(strip('connecting', LIVE), 'MichMesh.hub | Connecting… | warning+pulse | menu');
assert.strictEqual(strip('error', LIVE), 'MichMesh.hub | Session ended | error | menu');
assert.strictEqual(strip('offline', null), 'No hub yet | Network connected | online | status',
    'with no hub the strip is a status line, not a menu');
stripContext._anyInterfaceOnline = false;
assert.strictEqual(strip('offline', null), 'Not connected | No network | error | status');
stripContext._anyInterfaceOnline = null;
assert.strictEqual(strip('offline', null), 'No hub yet |  |  | status',
    'before the first stats the strip claims neither network state');
stripContext._anyInterfaceOnline = true;

// ---------------------------------------------------------------- hub menu

var menuContext = Object.assign({}, stripContext);
menuContext._channelsNetworkOnline = stripContext._channelsNetworkOnline;
vm.runInNewContext(
    sourceRange('_channelsMergedHubs', '_channelsHubSwitcherModel') +
        sourceRange('_channelsSelectedHubDestination', '_channelsCurrentHubObserved') +
        sourceRange('_channelsConnectCommandBlocked', '_channelsHubConnectMode') +
        sourceRange('_channelsHubDistance', '_channelsTimeLabel') +
        sourceRange('_channelsHubMenuCurrentMeta', '_channelsHubMenuKey'),
    menuContext,
    { filename: 'channels-hub-menu.js' }
);
menuContext._channelsIsConnecting = stripContext._channelsIsConnecting;

function menuItems(phase, online) {
    stripContext._anyInterfaceOnline = online === undefined ? true : online;
    menuContext.channelsSavedHubs = [{ destination_hash: HUB_C, label: 'Archive relay', nickname: 'rat', last_connected: 10 }];
    menuContext.channelsDiscoveredHubs = [
        { destination_hash: HUB_A, announced_name: 'Home', hops: 0, last_seen: 100 },
        { destination_hash: HUB_B, announced_name: 'Field', hops: 2, last_seen: 90 }
    ];
    menuContext.channelsSnapshot = {
        phase: phase,
        generation: 1,
        selected_hub_destination: HUB_A,
        hub: { destination_hash: HUB_A, name: 'Home', hops: 0 }
    };
    stripContext.channelsSnapshot = menuContext.channelsSnapshot;
    var chosen = [];
    var items = menuContext._channelsHubMenuItems(function(action, hub) {
        chosen.push(action + (hub ? ':' + hub.destination_hash : ''));
    });
    return {
        items: items,
        chosen: chosen,
        labels: Array.prototype.map.call(items, function(item) {
            if (item.separator) return '--';
            return item.label + (item.hint ? ' / ' + item.hint : '') + (item.checked ? ' (checked)' : '') +
                (item.disabled ? ' (disabled)' : '');
        })
    };
}

var active = menuItems('active');
assert.deepStrictEqual(Array.from(active.labels), [
    'Home / Connected · Direct (checked)',
    'Field / 2 hops',
    'Archive relay / Saved',
    '--',
    'Add a hub by address…',
    'Share this hub',
    'Hub details',
    '--',
    'Disconnect'
], 'one hub menu: switch hubs, add by address, share, details, disconnect');
active.items[1].onSelect();
assert.deepStrictEqual(Array.from(active.chosen), ['switch:' + HUB_B],
    'choosing another hub asks to switch to it');
assert.strictEqual(active.items[active.items.length - 1].danger, true,
    'Disconnect is the menu’s danger item');
assert.strictEqual(active.items[0].checked, true);
assert.strictEqual(active.items[1].checked, false, 'hub items are radio items: one is checked');

var pending = menuItems('connecting');
assert(pending.labels.indexOf('Field / 2 hops (disabled)') !== -1,
    'other hubs wait while a connection is in progress');
assert.strictEqual(pending.labels[pending.labels.length - 1], 'Cancel connection');
assert.strictEqual(pending.labels[0], 'Home / Connecting… (checked)');

var ended = menuItems('error');
assert.deepStrictEqual(Array.from(ended.labels).slice(-2), ['Reconnect', 'Disconnect'],
    'an ended session offers Reconnect before Disconnect');

var offline = menuItems('active', false);
assert(offline.labels.indexOf('Add a hub by address… (disabled)') !== -1,
    'an address cannot be reached without a network');
stripContext._anyInterfaceOnline = true;

var stripPosition = indexSource.indexOf('id="channel-hub-strip"');
var listPosition = indexSource.indexOf('id="channels-list"');
assert(stripPosition !== -1 && stripPosition < listPosition,
    'the hub strip sits above the channel list');
['channels-connect-btn', 'channel-hub-menu-btn', 'channel-room-menu-btn', 'channels-join-btn',
    'channel-hub-switcher-chevron', 'channel-hub-add-btn', 'channel-live-beacon'].forEach(function(id) {
    assert(indexSource.indexOf(id) === -1, 'no bare "+" or "…" buttons remain: ' + id);
});
assert(channelsSource.indexOf('function channelsOpenHubMenu(trigger)') !== -1);
assert(channelsSource.indexOf('function channelsOpenHubSwitcher') === -1 &&
    channelsSource.indexOf('function channelsOpenHubOptions') === -1,
    'the hub menu replaces the switcher sheet and the hub options menu');
assert(channelHubSource.indexOf('function channelsOpenAddSheet') === -1,
    'the Manage Hub sheet behind the header "+" is gone');
assert(channelsSource.indexOf('Only one hub can be connected at a time.') !== -1);
assert(channelsSource.indexOf('History stays on this device.') !== -1);
assert(channelsSource.indexOf("titleElement.textContent = 'Switch to '") !== -1);
assert(channelsSource.indexOf("sharedRoom ? 'Switch and review' : 'Switch'") !== -1);
assert(channelsSource.indexOf("'Switching channel hub\\u2026'") === -1,
    'the hub snapshot owns switching progress without a duplicate toast');
assert(channelsSource.indexOf("switching: connectMode.kind === 'switch'") !== -1);
assert(channelsSource.indexOf("'Could not switch channel hubs.'") !== -1);
assert(channelsSource.indexOf('openedEpoch === _channelsHistoryEpoch') !== -1);
assert(channelsSource.indexOf('openedGeneration === (Number(channelsSnapshot.generation) || 0)') !== -1);
assert(channelsSource.indexOf('if (_channelsHubMenu) RS.kit.closeMenu();') !== -1 &&
    channelsSource.indexOf('RS.kit.menu(trigger, {') !== -1,
    'an identity change must close the old identity’s hub menu');
assert(channelsSource.indexOf("RS.invoke('connect_channel_hub'") !== -1);
assert(channelsSource.indexOf('CHANNELS_CONNECTION_BUDGET') === -1,
    'the frontend must not invent or raise the runtime connection budget');

var menuSource = sourceRange('channelsOpenHubMenu', '_channelsRefreshHubMenu');
var connectSource = sourceRange('channelsOpenConnectSheet', '_channelsSheetField');
assert(menuSource.indexOf('localStorage') === -1);
assert(menuSource.indexOf("RS.invoke('connect_channel_hub'") === -1,
    'choosing a hub must open explicit review instead of connecting from the menu');
assert(menuSource.indexOf('channelsRefreshAvailableHubs()') !== -1,
    'the menu reads the announce cache when it opens');
assert(connectSource.indexOf('Only one hub can be connected at a time.') !== -1,
    'the one-hub sentence belongs to the switch confirmation');
assert(connectSource.indexOf('channel-connection-trust') === -1);
assert(connectSource.indexOf("initialMode.kind === 'current'") !== -1 &&
    connectSource.indexOf('channelsOpenHubDetails();') !== -1,
    'reviewing the current hub leads to its details instead of a disabled dead end');
assert(channelHubSource.indexOf(
    'return !!(overview && overview.supported && _channelHubHostingEnabled(overview));'
) !== -1,
    'Settings ON must reveal the first-run hosting card before a hub exists');

assert(cssSource.indexOf('.ch-hub-menu') === -1, 'no channels-local menu styling remains');
assert(cssSource.indexOf('@keyframes channelHubSignalLap') === -1 &&
    channelsSource.indexOf('link-arrived') === -1,
    'the status dot carries link state; the perimeter lap is gone');
assert(cssSource.indexOf('.channel-hub-switch-impact') !== -1);
assert(cssSource.indexOf('.channel-connection-trust') === -1);

console.log('channel hub switcher tests passed');
