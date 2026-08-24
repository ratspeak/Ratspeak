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
['waiting_for_radio', 'reconnecting'].forEach(function(state) {
    assert.strictEqual(
        context._mobileRnodeHealth('ble://00:11:22:33:44:55', {
            ble_rnode: { state: state }
        }).label,
        'Waiting for radio'
    );
});
assert.strictEqual(
    context._mobileRnodeHealth('ble://00:11:22:33:44:55', {
        ble_rnode: { state: 'initializing' }
    }).label,
    'Initializing'
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

var actionStart = source.indexOf('function buildIfaceActionItems');
var actionEnd = source.indexOf('\nfunction setInterfacePaused', actionStart);
assert(actionStart >= 0 && actionEnd > actionStart, 'interface action reducer must exist');
var actionCalls = [];
var actionRecord = {
    ifaceType: 'rnode',
    iface: { port: 'ble://00:11:22:33:44:55', enabled: true }
};
var actionContext = {
    ICON_PLAY: 'play', ICON_PAUSE: 'pause', ICON_RADIO: 'radio',
    ICON_PENCIL: 'pencil', ICON_TRASH: 'trash',
    _cachedConfigIfaces: { mobile_hardware: {} },
    getConfiguredInterfaceRecord: function() { return actionRecord; },
    getInterfaceLiveStatus: function() { return null; },
    lastStats: null,
    isInterfaceConfigEnabled: function(iface) { return iface.enabled !== false; },
    isLoraInterfaceType: function() { return true; },
    isTcpInterfaceType: function() { return false; },
    setInterfacePaused: function(type, name, paused) { actionCalls.push([type, name, paused]); },
    openInterfaceEdit: function() {},
    openRenameInterfaceDialog: function() {},
    confirmRemoveInterface: function() {},
};
vm.runInNewContext(source.slice(actionStart, actionEnd), actionContext, {
    filename: 'interface-action-items.js'
});

['waiting_for_radio', 'reconnecting', 'connecting', 'initializing'].forEach(function(state) {
    actionContext._cachedConfigIfaces.mobile_hardware.ble_rnode = { state: state };
    var item = actionContext.buildIfaceActionItems('rnode', 'Radio')[0];
    assert.strictEqual(item.label, 'Connecting…');
    assert.strictEqual(item.disabled, true);
    assert.strictEqual(item.onSelect, undefined);
});

actionContext._cachedConfigIfaces.mobile_hardware.ble_rnode = { state: 'failed' };
var retry = actionContext.buildIfaceActionItems('rnode', 'Radio')[0];
assert.strictEqual(retry.label, 'Retry Connection');
retry.onSelect();
assert.deepStrictEqual(actionCalls.pop(), ['rnode', 'Radio', false]);

actionContext._cachedConfigIfaces.mobile_hardware.ble_rnode = { state: 'conflict' };
var conflict = actionContext.buildIfaceActionItems('rnode', 'Radio')[0];
assert.strictEqual(conflict.label, 'Radio conflict');
assert.strictEqual(conflict.disabled, true);

actionContext._cachedConfigIfaces.mobile_hardware.ble_rnode = { state: 'connected' };
var pause = actionContext.buildIfaceActionItems('rnode', 'Radio')[0];
assert.strictEqual(pause.label, 'Pause Interface');
pause.onSelect();
assert.deepStrictEqual(actionCalls.pop(), ['rnode', 'Radio', true]);

actionRecord.iface.enabled = false;
var resume = actionContext.buildIfaceActionItems('rnode', 'Radio')[0];
assert.strictEqual(resume.label, 'Resume Interface');
resume.onSelect();
assert.deepStrictEqual(actionCalls.pop(), ['rnode', 'Radio', false]);

console.log('Mobile hardware health tests passed');
