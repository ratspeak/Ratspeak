#!/usr/bin/env node
// Deterministic model and source coverage for the one-live-hub switcher.

'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var root = path.join(__dirname, '..');
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
    var start = channelsSource.indexOf('function ' + startName);
    var end = channelsSource.indexOf('\nfunction ' + endName, start);
    assert(start !== -1 && end !== -1, startName + ' source range must exist');
    return channelsSource.slice(start, end);
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

var hubStripClasses = new Set();
var hubStrip = {
    dataset: { phase: 'offline' },
    classList: {
        add: function(name) { hubStripClasses.add(name); },
        remove: function(name) { hubStripClasses.delete(name); }
    }
};
var hubMenu = { hidden: true };
var hubSummary = {
    title: '',
    attributes: {},
    setAttribute: function(name, value) { this.attributes[name] = String(value); }
};
var hubStripText = {};
var animationFrames = [];
var intervals = [];
var clearedIntervals = [];
var stripContext = {
    CHANNEL_HUB_PULSE_INTERVAL_MS: 60 * 1000,
    _channelsHubPulseTimer: null,
    channelsSnapshot: {
        phase: 'active',
        nickname: 'Bob',
        last_error: null,
        hub: { destination_hash: HUB_A, name: 'MichMesh.hub' }
    },
    _channelsEl: function(id) {
        if (id === 'channel-hub-strip') return hubStrip;
        if (id === 'channel-hub-summary') return hubSummary;
        if (id === 'channel-hub-menu-btn') return hubMenu;
        return null;
    },
    _channelsSetText: function(id, value) { hubStripText[id] = value; },
    _channelsPhaseLabel: function(phase) { return phase; },
    _channelsHubName: function(hub) { return hub.name; },
    _channelsIsConnecting: function() { return false; },
    _channelsViewVisible: function() { return true; },
    _channelsShortHash: function(hash) { return hash.slice(0, 8); },
    requestAnimationFrame: function(callback) { animationFrames.push(callback); },
    setInterval: function(callback, delay) {
        intervals.push({ callback: callback, delay: delay });
        return intervals.length;
    },
    clearInterval: function(id) { clearedIntervals.push(id); },
    window: {
        matchMedia: function() { return { matches: false }; }
    }
};
vm.runInNewContext(
    sourceRange('_channelsHubPulseEnabled', '_channelsRenderList'),
    stripContext,
    { filename: 'channels-hub-strip.js' }
);

stripContext._channelsRenderHubStrip();
assert.strictEqual(hubStrip.dataset.phase, 'active');
assert.strictEqual(hubSummary.title, 'Hub options');
assert.strictEqual(
    hubSummary.attributes['aria-label'],
    'Hub options. MichMesh.hub. Connected as Bob'
);
assert.strictEqual(hubSummary.attributes['aria-haspopup'], 'menu');
assert.strictEqual(hubMenu.hidden, false,
    'Manage Hub must remain available from the ellipsis in every connection state');
assert.strictEqual(animationFrames.length, 1,
    'entering active queues one signal lap');
animationFrames.shift()();
assert(hubStripClasses.has('link-arrived'));
assert.strictEqual(intervals.length, 1,
    'active connectivity schedules one quiet periodic signal');
assert.strictEqual(intervals[0].delay, 60 * 1000,
    'the connected perimeter rests for a full minute between signals');
stripContext._channelsRenderHubStrip();
assert.strictEqual(animationFrames.length, 0,
    'routine active snapshots must not replay the signal lap');
assert.strictEqual(intervals.length, 1,
    'routine snapshots must not multiply connectivity timers');
hubStripClasses.delete('link-arrived');
intervals[0].callback();
assert.strictEqual(animationFrames.length, 1,
    'the minute signal queues one lightweight perimeter animation');
animationFrames.shift()();
assert(hubStripClasses.has('link-arrived'));
stripContext.channelsSnapshot.phase = 'stale';
stripContext._channelsRenderHubStrip();
assert(!hubStripClasses.has('link-arrived'),
    'leaving active clears any pending trace state');
assert.deepStrictEqual(clearedIntervals, [1],
    'leaving active cancels the periodic signal timer');

var stripPosition = indexSource.indexOf('id="channel-hub-summary"');
var listPosition = indexSource.indexOf('id="channels-list"');
assert(stripPosition !== -1 && stripPosition < listPosition,
    'the hub summary must remain visibly above the selected hub channel list');
assert(indexSource.indexOf('channel-hub-switcher-chevron') === -1,
    'the full hub card replaces a separate switcher chevron');
assert(indexSource.indexOf('channel-hub-add-btn') === -1,
    'mobile must not retain a redundant hub-card plus button');
assert(indexSource.indexOf('data-channel-action="hub-actions"') !== -1,
    'the hub summary must open actions for the current hub');
assert(indexSource.indexOf('id="channel-hub-menu-btn" type="button" title="Manage Hub"') !== -1,
    'the ellipsis must own hub selection and connection management');
assert(indexSource.indexOf('channel-live-beacon') === -1,
    'the connected perimeter replaces the redundant status dot');
assert(channelsSource.indexOf('function channelsOpenHubSwitcher()') !== -1);
assert(channelsSource.indexOf("hubSwitcher.addEventListener('click', channelsOpenHubSwitcher)") === -1);
assert(channelsSource.indexOf('One hub can be live at a time.') !== -1);
assert(channelsSource.indexOf('history stays on this device.') !== -1);
assert(channelsSource.indexOf("list.setAttribute('aria-live', 'polite')") !== -1);
assert(channelsSource.indexOf("list.setAttribute('aria-busy', 'true')") !== -1);
assert(channelsSource.indexOf("titleElement.textContent = 'Switch to '") !== -1);
assert(channelsSource.indexOf("sharedRoom ? 'Switch and review' : 'Switch'") !== -1);
assert(channelsSource.indexOf("'Switching channel hub\\u2026'") !== -1);
assert(channelsSource.indexOf("switching: connectMode.kind === 'switch'") !== -1);
assert(channelsSource.indexOf("'Could not switch channel hubs.'") !== -1);
assert(channelsSource.indexOf("case 'reconnecting': return { label: 'Reconnecting'") !== -1);
assert(channelsSource.indexOf('openedEpoch === _channelsHistoryEpoch') !== -1);
assert(channelsSource.indexOf('openedGeneration === (Number(channelsSnapshot.generation) || 0)') !== -1);
assert(channelsSource.indexOf('dismissHubSwitcher()') !== -1,
    'an identity change must close the old identity switcher');
assert(channelsSource.indexOf("RS.invoke('connect_channel_hub'") !== -1);
assert(channelsSource.indexOf('CHANNELS_CONNECTION_BUDGET') === -1,
    'the frontend must not invent or raise the runtime connection budget');

var switcherSource = sourceRange('channelsOpenHubSwitcher', 'channelsOpenConnectSheet');
var connectSource = sourceRange('channelsOpenConnectSheet', '_channelsSheetField');
assert(switcherSource.indexOf('localStorage') === -1);
assert(switcherSource.indexOf("RS.invoke('connect_channel_hub'") === -1,
    'choosing a hub must open explicit review instead of connecting from the switcher');
assert(switcherSource.indexOf('channelsRefreshAvailableHubs()') !== -1);
assert(switcherSource.indexOf('Scan') === -1,
    'the recent announce cache must not be presented as an active network scan');
assert(connectSource.indexOf('Available hubs') === -1,
    'connection review must not repeat the hub picker');
assert(connectSource.indexOf('Open a shared channel') === -1,
    'link acquisition belongs in Manage Hub, not connection review');
assert(connectSource.indexOf('Encrypted in transit') === -1,
    'connection review must not repeat transport trust copy');
assert(connectSource.indexOf('Ends live rooms') === -1,
    'the switch title and action are sufficient confirmation');
assert(connectSource.indexOf('channel-connection-trust') === -1,
    'removed connection copy must not leave an empty layout row');
assert(connectSource.indexOf("initialMode.kind === 'current'") !== -1 &&
    connectSource.indexOf('channelsOpenHubOptions();') !== -1,
    'reviewing the current hub must lead to hub actions instead of a disabled dead end');
assert(channelHubSource.indexOf("title: 'Manage Hub'") !== -1);
assert(channelHubSource.indexOf("'Use a link or QR'") !== -1);
assert(channelHubSource.indexOf(
    'if (overview.supported && _channelHubHostingEnabled(overview))'
) !== -1,
    'hosting requires Settings opt-in, but Manage Hub must remain available everywhere');
assert(channelHubSource.indexOf(
    'return !!(overview && overview.supported && _channelHubHostingEnabled(overview));'
) !== -1,
    'Settings ON must reveal the first-run hosting card before a hub exists');

assert(cssSource.indexOf('.channel-hub-summary') !== -1);
assert(cssSource.indexOf('.channel-live-beacon') === -1);
assert(cssSource.indexOf('@keyframes channelHubSignalLap') !== -1);
assert(cssSource.indexOf('animation: channelHubSignalLap 2.25s linear') !== -1,
    'the connected trace must read as a deliberate lap rather than a fast flicker');
assert(cssSource.indexOf('@media (prefers-reduced-motion: reduce)') !== -1);
assert(channelsSource.indexOf("previousPhase !== 'active'") !== -1,
    'the signal lap must run only on a transition into the active state');
assert(cssSource.indexOf('.channel-hub-switcher-list .channel-hub-row.current') !== -1);
assert(cssSource.indexOf('.channel-hub-switch-impact') !== -1);
assert(cssSource.indexOf('.channel-connection-trust') === -1);

console.log('channel hub switcher tests passed');
