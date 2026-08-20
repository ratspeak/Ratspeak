#!/usr/bin/env node
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var source = fs.readFileSync(
    path.join(__dirname, '..', 'static', 'js', 'health.js'),
    'utf8'
);
var start = source.indexOf('function _mobileRnodeHealth');
var end = source.indexOf('\nfunction _interfaceConfigByName', start);
assert(start >= 0 && end > start, 'mobile hardware health reducer must exist');
var context = {
    isInterfaceConfigEnabled: function(iface) { return iface.enabled !== false; }
};
vm.runInNewContext(source.slice(start, end), context, { filename: 'mobile-hardware-health.js' });

assert.deepStrictEqual(
    JSON.parse(JSON.stringify(context._mobileRnodeHealth('androidusb://opaque', {
        usb_rnode: { state: 'permission_needed' }
    }))),
    {
        kind: 'usb_rnode', state: 'permission_needed', reason: '',
        label: 'USB permission needed', actionable: true
    }
);
assert.deepStrictEqual(
    JSON.parse(JSON.stringify(context._mobileRnodeHealth('ble://00:11:22:33:44:55', {
        ble_rnode: { state: 'failed', reason: 'bluetooth_off' }
    }))),
    {
        kind: 'ble_rnode', state: 'failed', reason: 'bluetooth_off',
        label: 'Bluetooth is off', actionable: true
    }
);
assert.strictEqual(
    context._mobileRnodeHealth('ble://00:11:22:33:44:55', {
        ble_rnode: { state: 'connected' }
    }),
    null,
    'healthy BLE must not retain a warning pill'
);
assert.deepStrictEqual(
    JSON.parse(JSON.stringify(context._configuredInterfaceFallback({
        ifaceType: 'rnode',
        iface: { port: '/dev/cu.usbserial-0001', enabled: true }
    }, {}))),
    {
        paused: false, waitingForDevice: false, mobileHealth: null, connecting: true
    },
    'a configured serial radio must render as connecting before live stats arrive'
);
assert.deepStrictEqual(
    JSON.parse(JSON.stringify(context._configuredInterfaceFallback({
        ifaceType: 'rnode',
        iface: { port: '/dev/cu.usbserial-0001', enabled: false }
    }, {}))),
    {
        paused: true, waitingForDevice: false, mobileHealth: null, connecting: false
    },
    'a paused configured radio must remain visible without live stats'
);
assert(source.includes('if (online) mobileHealth = null;'),
    'ambient USB/BLE state must never override exact online transport evidence');
assert(source.includes('function applyMobileHardwareState(data)'),
    'live native state must update the retained interface projection');

console.log('Mobile hardware health tests passed');
