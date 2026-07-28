#!/usr/bin/env node
// Deterministic tests for channel hub list ordering: closer hubs rank higher.

'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var channelsPath = path.join(__dirname, '..', 'static', 'js', 'channels.js');
var channelsSource = fs.readFileSync(channelsPath, 'utf8');
var indexSource = fs.readFileSync(path.join(__dirname, '..', 'index.html'), 'utf8');

function sourceFunction(name, nextName) {
    var start = channelsSource.indexOf('function ' + name);
    var end = channelsSource.indexOf('\nfunction ' + nextName, start);
    assert(start !== -1 && end !== -1, name + ' must exist');
    return channelsSource.slice(start, end);
}

var mergeSource = sourceFunction('_channelsMergedHubs', '_channelsSetText');

function merge(saved, discovered) {
    var context = {
        channelsSavedHubs: saved,
        channelsDiscoveredHubs: discovered
    };
    vm.runInNewContext(mergeSource + '\nthis.merged = _channelsMergedHubs();', context);
    return context.merged;
}

// The merged list is built inside the vm realm, so compare order as a string
// rather than deep-comparing cross-realm arrays.
function hashesOf(hubs) {
    return Array.prototype.map.call(hubs, function(hub) {
        return hub.destination_hash;
    }).join(',');
}

// Heard hubs sort by hop count ascending, regardless of which was heard most
// recently — a three-hop hub must never outrank a one-hop hub.
var order = merge([], [
    { destination_hash: 'far', hops: 5, last_seen: 9000 },
    { destination_hash: 'near', hops: 1, last_seen: 100 },
    { destination_hash: 'mid', hops: 3, last_seen: 5000 }
]);
assert.strictEqual(hashesOf(order), 'near,mid,far',
    'fewer hops must rank higher');

// Equal hop counts fall back to recency.
var tie = merge([], [
    { destination_hash: 'older', hops: 2, last_seen: 100 },
    { destination_hash: 'newer', hops: 2, last_seen: 9000 }
]);
assert.strictEqual(hashesOf(tie), 'newer,older',
    'equal hops fall back to most recently heard');

// A saved hub we have not heard has no hop count; heard hubs outrank it.
var mixed = merge(
    [{ destination_hash: 'saved', label: 'Saved', nickname: '', last_connected: 9999 }],
    [{ destination_hash: 'heard', hops: 4, last_seen: 1 }]
);
assert.strictEqual(hashesOf(mixed), 'heard,saved',
    'a hub we can currently hear outranks a saved one we cannot');

// A saved hub that is also being heard keeps its bookmark data and its hops.
var both = merge(
    [{ destination_hash: 'dual', label: 'My relay', nickname: 'Rat', last_connected: 10 }],
    [{ destination_hash: 'dual', hops: 2, last_seen: 20, announced_name: 'Relay' }]
);
assert.strictEqual(both.length, 1, 'the same hub must not appear twice');
assert.strictEqual(both[0].hops, 2);
assert.strictEqual(both[0].label, 'My relay');
assert.strictEqual(both[0].saved, true);
assert.strictEqual(both[0].nearby, true);

// Zero hops (a directly attached hub) must sort ahead of one hop, not be
// treated as missing.
var direct = merge([], [
    { destination_hash: 'one', hops: 1, last_seen: 500 },
    { destination_hash: 'zero', hops: 0, last_seen: 1 }
]);
assert.strictEqual(hashesOf(direct), 'zero,one',
    'a zero-hop hub is the closest, not an unknown distance');

var presentationContext = {
    _channelsHubName: function(hub) { return hub.announced_name || 'Channel hub'; },
    _channelsShortHash: function(value) { return 'short:' + value; }
};
vm.runInNewContext(
    sourceFunction('_channelsHubMonogram', '_channelsBuildHubRow') +
        '\nthis.monogram = _channelsHubMonogram;' +
        '\nthis.distance = _channelsHubDistance;' +
        '\nthis.meta = _channelsHubMeta;',
    presentationContext
);
assert.strictEqual(presentationContext.monogram({ announced_name: 'fishy hub' }), 'F');
assert.strictEqual(presentationContext.monogram({ announced_name: 'Почен Брянск' }), 'П');
assert.strictEqual(presentationContext.distance({ nearby: true, hops: 0 }), 'Direct');
assert.strictEqual(presentationContext.distance({ nearby: true, hops: 1 }), '1 hop');
assert.strictEqual(presentationContext.distance({ nearby: true, hops: 4 }), '4 hops');
assert.strictEqual(presentationContext.distance({ saved: true, nearby: false }), '');
assert.strictEqual(
    presentationContext.meta({ destination_hash: 'abcd', saved: true, nearby: false }),
    'Saved · short:abcd'
);
assert.strictEqual(
    presentationContext.meta({ destination_hash: 'abcd', saved: true, nearby: true }),
    'short:abcd'
);

assert(indexSource.indexOf('Available hubs') !== -1,
    'the directory heading should describe the choices, not their cache provenance');
assert(indexSource.indexOf('channels-refresh-btn') === -1,
    'the announce cache must not be presented as an active network scan');
assert(channelsSource.indexOf("RS.listen('announce_received'") !== -1,
    'new announces should refresh the visible hub directory automatically');
assert(channelsSource.indexOf("hub.nearby ? 'Nearby' : 'Recent'") === -1,
    'each row should not repeat the directory heading as a status chip');

console.log('channel hub ordering tests passed');
