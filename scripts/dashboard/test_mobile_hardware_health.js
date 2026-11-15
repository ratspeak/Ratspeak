#!/usr/bin/env node
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var source = fs.readFileSync(
    path.join(__dirname, '..', '..', 'dashboard', 'static', 'js', 'health.js'),
    'utf8'
);
var start = source.indexOf('var RNODE_STATE_LABELS');
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
// A retrying radio keeps explaining its last failure through the next attempt.
[
    ['not_reachable', 'Radio not found'],
    ['link_lost', 'Connection dropped'],
    ['service_missing', 'No RNode service'],
    ['permission_needed', 'Bluetooth permission needed'],
    ['radio_disconnected', null],
    ['unknown_reason', null],
].forEach(function(row) {
    ['waiting_for_radio', 'connecting'].forEach(function(state) {
        var health = context._mobileRnodeHealth('ble://00:11:22:33:44:55', {
            ble_rnode: { state: state, reason: row[0] }
        });
        var generic = state === 'connecting' ? 'Connecting' : 'Waiting for radio';
        assert.strictEqual(health.label, row[1] || generic, state + '/' + row[0]);
        assert.strictEqual(health.actionable, row[0] === 'permission_needed', state + '/' + row[0]);
    });
});
assert.strictEqual(
    context._mobileRnodeHealth('ble://00:11:22:33:44:55', {
        ble_rnode: { state: 'failed', reason: 'stale_bond' }
    }).label,
    'Pair again'
);
[
    ['rejected_tx_power', 'TX power not supported'],
    ['rejected_frequency', 'Frequency not supported'],
    ['unsupported_firmware', 'Firmware update needed'],
].forEach(function(row) {
    var health = context._mobileRnodeHealth('ble://00:11:22:33:44:55', {
        ble_rnode: { state: 'failed', reason: row[0] }
    });
    assert.strictEqual(health.label, row[1], row[0]);
    assert.strictEqual(health.actionable, true, row[0]);
});
assert.strictEqual(
    context._mobileRnodeHealth('ble://00:11:22:33:44:55', {
        ble_rnode: { state: 'connected' }
    }),
    null,
    'healthy BLE must not retain a warning pill'
);
// Desktop and Android USB rows read the driver's own state from live stats.
[
    [{ state: 'failed', reason: 'rejected_tx_power' }, 'TX power not supported', true],
    [{ state: 'failed', reason: 'unsupported_radio' }, 'Radio not supported', true],
    [{ state: 'waiting_for_radio', reason: 'not_reachable' }, 'Radio not found', false],
    [{ state: 'waiting_for_radio', reason: 'link_lost' }, 'Connection dropped', false],
    [{ state: 'waiting_for_radio', reason: 'port_not_found' }, 'Radio not found', false],
    [{ state: 'waiting_for_radio', reason: 'port_access_denied' }, 'Port access denied', false],
    [{ state: 'waiting_for_radio', reason: 'port_in_use' }, 'Port in use', false],
    [{ state: 'initializing', reason: null }, 'Initializing', false],
    [{ state: 'connecting', reason: null }, 'Connecting', false],
].forEach(function(row) {
    var health = context._rnodeRuntimeHealth(row[0]);
    assert.strictEqual(health.label, row[1], JSON.stringify(row[0]));
    assert.strictEqual(health.actionable, row[2], JSON.stringify(row[0]));
});
assert.strictEqual(context._rnodeRuntimeHealth(null), null, 'a Ready radio has no row state');
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
var networkView = fs.readFileSync(
    path.join(__dirname, '..', '..', 'dashboard', 'static', 'js', 'network_view.js'),
    'utf8'
);
assert(networkView.includes("if (c.status === 'pending') return c.health || 'Connecting…';"),
    'ambient USB/BLE state explains only a connection that is not online');
assert(source.includes('function applyMobileHardwareState(data)'),
    'live native state must update the retained interface projection');

function inner(src, name) {
    var start = src.indexOf('    function ' + name + '(');
    assert(start >= 0, name + ' must exist');
    var end = src.indexOf('\n    }\n', start);
    return src.slice(start, end + 6);
}
var helpStart = source.indexOf('// Guidance for BLE RNode failures');
var helpEnd = source.indexOf('\nfunction setInterfacePaused', helpStart);
assert(helpStart >= 0 && helpEnd > helpStart, 'radio help must exist');
var dialogs = [];
var androidCalls = [];
var permissionCalls = [];
var edits = [];
var ctx = {
    window: {
        _cachedConfigIfaces: { mobile_hardware: {} },
        RatspeakAndroid: {
            openBluetoothSettings: function() { androidCalls.push('bluetooth'); return true; },
            openAppPermissionSettings: function() { androidCalls.push('app'); return true; },
        }
    },
    hasAndroidBridge: function() { return true; },
    rsAlert: function(opts) { dialogs.push(['alert', opts.title]); return Promise.resolve(); },
    rsConfirm: function(opts) { dialogs.push(['confirm', opts.title]); return Promise.resolve(true); },
    _ensureBlePermissions: function() { permissionCalls.push(true); return Promise.resolve(ctx.permissionGranted); },
    permissionGranted: true,
    getInterfaceLiveStatus: function() { return ctx.liveStatus; },
    liveStatus: null,
    _rnodeIsTcpPort: function(port) { return /^tcp:\/\//.test(port); },
};
vm.runInNewContext(source.slice(helpStart, helpEnd) + '\n' +
    ['radioVia', 'radioMhz', 'radioTrouble', 'editable', 'leaveWord', 'menuItems'].map(function(n) { return inner(networkView, n); }).join('\n'),
    ctx, { filename: 'network-radio-help.js' });

function radio(port, status) {
    return { id: 'Radio', kind: 'lora', ifaceType: 'rnode', status: status || 'pending', iface: { port: port, frequency: 915000000 } };
}
function ops(c) { return Array.from(ctx.menuItems(c)).filter(function(x) { return !x.separator; }).map(function(x) { return x.op; }); }
function trouble(state, reason, port) {
    ctx.window._cachedConfigIfaces.mobile_hardware = { ble_rnode: { state: state, reason: reason } };
    return ctx.radioTrouble(radio(port || 'ble://00:11:22:33:44:55'));
}

// #86: the detail reads the frequency and how the radio is connected.
assert.strictEqual(ctx.radioVia({ port: 'ble://00:11:22:33:44:55' }), 'Bluetooth');
assert.strictEqual(ctx.radioVia({ port: 'androidusb://opaque' }), 'USB');
assert.strictEqual(ctx.radioVia({ port: '/dev/cu.usbmodem4101' }), 'USB');
assert.strictEqual(ctx.radioVia({ port: 'tcp://10.0.0.5:7633' }), 'Wi-Fi');
assert.strictEqual(ctx.radioMhz({ frequency: 915000000 }), '915.000 MHz');

// #80: an enabled radio that is not connected yet still offers Pause; a paused one offers Resume.
['pending', 'up'].forEach(function(status) {
    assert(ops(radio('ble://00:11:22:33:44:55', status)).indexOf('pause') !== -1, status + ' radios offer Pause');
});
assert(ops(radio('/dev/cu.usbmodem4101', 'paused')).indexOf('resume') !== -1);
assert.strictEqual(ops(radio('/dev/cu.usbmodem4101', 'off')).indexOf('pause'), -1, 'a radio that is off is not paused');

// What a struggling radio needs from the person.
var unreachable = trouble('waiting_for_radio', 'not_reachable');
assert.strictEqual(unreachable.help.title, 'Radio not found');
assert(!unreachable.retry && !unreachable.permission);
[['radio_unresponsive', 'Radio not answering'], ['link_lost', 'Connection dropped']].forEach(function(row) {
    assert.strictEqual(trouble('connecting', row[0]).help.title, row[1], row[0]);
});
assert.strictEqual(trouble('initializing', 'radio_start_failed').help.title, 'Radio failed to start');
assert.strictEqual(trouble('initializing', 'link_limited').help.title, 'Bluetooth link too small');
[['link_limited', 'Bluetooth link too small'], ['pin_rejected', 'Pairing not accepted'],
    ['pairing_window_closed', 'Pairing interrupted'], ['stale_bond', 'Pair again'],
    ['bond_timeout', 'Pairing needed'], ['rejected_tx_power', 'TX power not supported']].forEach(function(row) {
    var t = trouble('failed', row[0]);
    assert.strictEqual(t.help.title, row[1], row[0]);
    assert(t.retry, row[0] + ' offers Retry');
});
var grant = trouble('connecting', 'permission_needed');
assert(grant.permission, 'a missing Bluetooth permission offers Allow Bluetooth');
[['connected', ''], ['initializing', ''], ['waiting_for_radio', 'radio_disconnected'], ['failed', 'connect_failed']].forEach(function(row) {
    var t = trouble(row[0], row[1]);
    assert(!t || !t.help, row.join('/'));
});
var pausedRadio = radio('ble://00:11:22:33:44:55', 'paused');
ctx.window._cachedConfigIfaces.mobile_hardware = { ble_rnode: { state: 'failed', reason: 'stale_bond' } };
assert.strictEqual(ctx.radioTrouble(pausedRadio), null, 'a paused radio has nothing to troubleshoot');
// A desktop serial RNode has no Android hardware state; its driver explains it.
ctx.window._cachedConfigIfaces.mobile_hardware = {};
function serial(state, reason) {
    ctx.liveStatus = { online: false, rnode_state: { state: state, reason: reason } };
    return ctx.radioTrouble(radio('/dev/cu.usbmodem4101'));
}
var serialRejected = serial('failed', 'rejected_tx_power');
assert(serialRejected.retry && serialRejected.help.title === 'TX power not supported');
var serialAway = serial('waiting_for_radio', 'not_reachable');
assert(!serialAway.retry && serialAway.help.title === 'Radio not found', 'wired radios get wired guidance');
assert.strictEqual(serial('waiting_for_radio', 'port_access_denied').help.title, 'Port access denied');
assert.strictEqual(serial('waiting_for_radio', 'port_in_use').help.title, 'Port in use');
ctx.liveStatus = null;

function flush() { return new Promise(function(resolve) { setImmediate(resolve); }); }

(async function() {
    ctx.showBleRnodeHelp(unreachable.help, function() { edits.push(true); });
    await flush();
    assert.deepStrictEqual(dialogs.pop(), ['alert', 'Radio not found']);

    ctx.showBleRnodeHelp(trouble('failed', 'stale_bond').help, function() {});
    await flush();
    assert.deepStrictEqual(dialogs.pop(), ['confirm', 'Pair again']);
    assert.strictEqual(androidCalls.pop(), 'bluetooth');

    // A rejected setting is fixed by editing the interface, not by retrying.
    ctx.showBleRnodeHelp(serialRejected.help, function() { edits.push(true); });
    await flush();
    assert.deepStrictEqual(dialogs.pop(), ['confirm', 'TX power not supported']);
    assert.strictEqual(edits.length, 1);

    ctx.requestBleRnodePermission();
    await flush();
    assert.strictEqual(permissionCalls.length, 1);
    assert.strictEqual(dialogs.length, 0, 'a granted permission needs no settings detour');

    // Android stops prompting after repeated denials; Settings is the only path.
    ctx.permissionGranted = false;
    ctx.requestBleRnodePermission();
    await flush();
    assert.deepStrictEqual(dialogs.pop(), ['confirm', 'Bluetooth permission needed']);
    assert.strictEqual(androidCalls.pop(), 'app');

    console.log('Mobile hardware health tests passed');
})().catch(function(error) {
    console.error(error);
    process.exit(1);
});
