#!/usr/bin/env node
// Deterministic tests for channel hub list ordering: closer hubs rank higher.

'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var channelsPath = path.join(__dirname, '..', 'static', 'js', 'channels.js');
var channelsSource = fs.readFileSync(channelsPath, 'utf8');

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

console.log('channel hub ordering tests passed');
