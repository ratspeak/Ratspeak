#!/usr/bin/env node
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var dashboardRoot = path.join(__dirname, '..', '..', 'dashboard');
var modalSource = fs.readFileSync(path.join(dashboardRoot, 'static', 'js', 'modals.js'), 'utf8');
var start = modalSource.indexOf('var _RNODE_INTERFACE_MODE_VALUES');
var end = modalSource.indexOf('\nfunction _rnodeDeveloperModeEnabled', start);
assert(start >= 0 && end > start, 'missing RNode interface-mode source');
// The mode is a kit Select field; its value lives with the dialog's other Select fields.
var selectStart = modalSource.indexOf('var _netdlgSelects = {};');
var selectEnd = modalSource.indexOf('\nfunction _netdlgSelectField', selectStart);
assert(selectStart >= 0 && selectEnd > selectStart, 'missing Select field source');

var context = {
    String: String,
    document: { getElementById: function() { return null; } }
};
vm.runInNewContext(modalSource.slice(selectStart, selectEnd) + modalSource.slice(start, end), context, { filename: 'rnode-interface-mode.js' });

assert.strictEqual(context._rnodeNormaliseInterfaceMode(null), 'full',
    'missing legacy values must retain the established Full fallback');
assert.strictEqual(context._rnodeInitialInterfaceMode(null), 'roaming',
    'a new RNode must start in Roaming mode');
assert.strictEqual(context._rnodeInitialInterfaceMode({ mode: 'full' }), 'full',
    'editing an explicit Full interface must not change its mode');
assert.strictEqual(context._rnodeInitialInterfaceMode({ interface_mode: 'boundary' }), 'boundary',
    'editing must accept the compatibility interface_mode field');
assert.strictEqual(context._rnodeInitialInterfaceMode({}), 'full',
    'editing legacy data without a mode must preserve the prior Full behavior');
var opener = modalSource.slice(modalSource.indexOf('function openRnodeModal('), modalSource.indexOf('function _rnodeDialogClosed('));
assert(opener.includes('_rnodeSetInterfaceMode(_rnodeInitialInterfaceMode(editIface));'),
    'the Add/Edit flow must apply the context-aware default before submission');
assert(modalSource.includes('mode: _rnodeReadInterfaceMode(),'),
    'both visible and hidden mode selections must be included in the submitted interface');

context._rnodeSetInterfaceMode(context._rnodeInitialInterfaceMode(null));
assert.strictEqual(context._netdlgSelectGet('rnode-interface-mode'), 'roaming', 'the new-interface default must reach the form control');
assert.strictEqual(context._rnodeReadInterfaceMode(), 'roaming');
context._rnodeSetInterfaceMode(context._rnodeInitialInterfaceMode({ mode: 'gateway' }));
assert.strictEqual(context._netdlgSelectGet('rnode-interface-mode'), 'gateway', 'an explicit edited mode must reach the form control');

// The control is developer-only, but its value is set either way, so a hidden choice is still sent.
assert(modalSource.includes("{ key: 'roaming', label: 'Roaming (recommended)' }"),
    'the visible recommendation must be Roaming');
assert(modalSource.includes("_netdlgSelectField('rnode-interface-mode', 'Interface mode',\n            'Best for mobile radios. Choose another mode only when this RNode has a fixed routing role.',\n            ' id=\"rnode-mode-field\" data-netdlg-dev' + (_developerModeEnabled() ? '' : ' hidden'))"),
    'the advanced mode control must expose its radio-specific guidance, behind developer mode');
var html = fs.readFileSync(path.join(dashboardRoot, 'index.html'), 'utf8');
assert(html.includes('Shows advanced connection settings.'),
    'Developer Mode settings copy must explain that it reveals interface controls');

var nameStart = modalSource.indexOf('function _rnodeDefaultInterfaceName');
var nameEnd = modalSource.indexOf('\nfunction rnodeUpdateNextBtn', nameStart);
assert(nameStart >= 0 && nameEnd > nameStart, 'missing RNode default-name normalizer');
var nameContext = { String: String };
vm.runInNewContext(modalSource.slice(nameStart, nameEnd), nameContext, {
    filename: 'rnode-default-name.js'
});
assert.strictEqual(nameContext._rnodeDefaultInterfaceName('RNode a8eb'), 'RNode_A8EB',
    'advertised BLE identifiers must use the product default interface shape');
assert.strictEqual(nameContext._rnodeDefaultInterfaceName('RNode BFC5'), 'RNode_BFC5',
    'the observed firmware advertisement must remain the canonical product identifier');
assert.strictEqual(nameContext._rnodeDefaultInterfaceName('Unknown device'), 'RNode',
    'unexpected local names must not become product identifiers');
assert(!modalSource.includes("RS.invoke('api_rnode_default_name'"),
    'serial setup must not substitute the unrelated provisioning serial');
assert(modalSource.includes("nameInput.value = 'RNode';"),
    'USB-only setup must use an honest editable fallback when no advertisement is observable');

console.log('RNode interface mode tests passed');
