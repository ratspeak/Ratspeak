#!/usr/bin/env node
'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const source = fs.readFileSync(path.join(__dirname, '../../dashboard/static/js/lxmf.js'), 'utf8');
const names = ['_isTerminalMessageState', '_messageDeliveryMethod', '_messageCanCancelSend',
    '_messageHasTransferPayload', '_messageCanCancelTransfer', '_messageTransferPayloadSize',
    '_messageShowsTransferPercent', '_messageProgressPercent'];
const context = vm.createContext({lxmfLimits: {efficient_resource_bytes: 1048575}});
for (const name of names) {
    const start = source.indexOf('function ' + name + '(');
    const end = source.indexOf('\nfunction ', start + 1);
    assert(start >= 0 && end > start, name);
    vm.runInContext(source.slice(start, end), context);
}
const message = {direction: 'outbound', state: 'resource_transferring', delivery_method: 'direct',
    image: {size: 3500000}};
const percent = (progress, changes = {}) => context._messageProgressPercent({...message,
    delivery_progress: progress, ...changes});
for (const progress of [0.03, 0.05, 0.10, 0.101]) assert.equal(percent(progress), 1);
assert.equal(percent(0.19), 10);
assert.equal(percent(0.55), 50);
assert.equal(percent(0.91), 90);
assert.equal(percent(0.99), 99, 'awaiting proof must not claim completion');
assert.equal(percent(0.99999), 99);
for (const progress of [undefined, null, NaN, Infinity, -1, 0, 1]) assert.equal(percent(progress), null);
for (const state of ['delivered', 'failed', 'cancelled', 'rejected', 'timeout', 'propagated']) {
    assert.equal(percent(0.55, {state}), null);
}
assert.equal(percent(0.55, {image: {size: 100000}}), null);
assert.equal(percent(0.55, {direction: 'inbound'}), null);
assert.equal(percent(0.55, {image: null, attachments: [{size: 2000000}, {size: 1500000}]}), 50);
assert.equal(percent(0.55, {delivery_method: 'propagated'}), 50);
console.log('Transfer progress: setup, payload phase, proof cap, terminal states and attachment sizes passed');
