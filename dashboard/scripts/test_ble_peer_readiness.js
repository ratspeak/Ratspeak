#!/usr/bin/env node
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var root = path.join(__dirname, '..', '..');
var health = fs.readFileSync(path.join(root, 'dashboard/static/js/health.js'), 'utf8');
var events = fs.readFileSync(path.join(root, 'dashboard/static/js/tauri_events.js'), 'utf8');
var backend = fs.readFileSync(path.join(root, 'crates/ratspeak-tauri/src/commands/ble.rs'), 'utf8');

var labelStart = health.indexOf('function _resolveBlePeerLabel');
var labelEnd = health.indexOf('\nfunction _blePeerRepresentativeScore', labelStart);
var labelContext = { PeersCache: { get: function() { return null; } } };
vm.runInNewContext(health.slice(labelStart, labelEnd), labelContext);
assert.deepStrictEqual(
    JSON.parse(JSON.stringify(labelContext._resolveBlePeerLabel({ address: 'AA:BB:CC:DD:EE:FF' }))),
    { label: 'Identifying peer\u2026', title: 'Awaiting a signed identity announce' },
    'an unauthenticated transport address must never be rendered as peer identity'
);

var visibleStart = health.indexOf('function _blePeerRepresentativeScore');
var visibleEnd = health.indexOf('\nfunction renderBlePeerRow', visibleStart);
var visibleContext = {
    window: {
        _blePeers: {
            transportOnly: {
                address: 'AA:BB:CC:DD:EE:FF', connected: true,
                identity_hash: '', routable: false
            },
            verified: {
                address: '11:22:33:44:55:66', connected: true,
                identity_hash: '11111111111111111111111111111111', routable: true,
                protocol: 'Ratspeak'
            }
        }
    }
};
vm.runInNewContext(health.slice(visibleStart, visibleEnd), visibleContext);
assert.strictEqual(visibleContext.window._bleVisiblePeersFromCache().length, 1,
    'only signed, routable peers may appear in the active peer list');

assert(events.includes("provisional_identity_hash: provisionalIdentity"));
assert(events.includes("routable: !!identity && data.routable !== false"));
assert(events.includes("p.routable !== true"));
assert(!health.includes("return { label: addr, title: addr }"));
assert(backend.includes('BLE_IDENTITY_ANNOUNCE_RETRY_DELAYS_SECS'));
assert(backend.includes('"readiness": if routable { "routable" } else { "connected" }'));
assert(backend.includes('"readiness": "routable"'));
assert(backend.includes('verified_ble_peer_rows(address_to_identity)'));

console.log('BLE peer readiness tests passed');
