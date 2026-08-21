#!/usr/bin/env node
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var dashboardRoot = path.join(__dirname, '..');
var modalSource = fs.readFileSync(path.join(dashboardRoot, 'static', 'js', 'modals.js'), 'utf8');
var html = fs.readFileSync(path.join(dashboardRoot, 'index.html'), 'utf8');
var start = modalSource.indexOf('var _RNODE_INTERFACE_MODE_VALUES');
var end = modalSource.indexOf('\nfunction _rnodeDeveloperModeEnabled', start);
assert(start >= 0 && end > start, 'missing RNode interface-mode source');

var select = { value: '' };
var context = {
    String: String,
    document: {
        getElementById: function(id) {
            return id === 'rnode-interface-mode' ? select : null;
        }
    }
};
vm.runInNewContext(modalSource.slice(start, end), context, { filename: 'rnode-interface-mode.js' });

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
assert(modalSource.includes('_rnodeSetInterfaceMode(_rnodeInitialInterfaceMode(editIface));'),
    'the Add/Edit flow must apply the context-aware default before submission');
assert(modalSource.indexOf('_rnodeSetInterfaceMode(_rnodeInitialInterfaceMode(editIface));') <
    modalSource.indexOf('_rnodeSyncInterfaceModeVisibility();'),
    'the selected mode must be initialized even when Developer Mode hides the control');
assert(modalSource.includes('mode: _rnodeReadInterfaceMode(),'),
    'both visible and hidden mode selections must be included in the submitted interface');

context._rnodeSetInterfaceMode(context._rnodeInitialInterfaceMode(null));
assert.strictEqual(select.value, 'roaming', 'the new-interface default must reach the form control');
context._rnodeSetInterfaceMode(context._rnodeInitialInterfaceMode({ mode: 'gateway' }));
assert.strictEqual(select.value, 'gateway', 'an explicit edited mode must reach the form control');

assert(/<option value="roaming" selected>Roaming \(recommended\)<\/option>/.test(html),
    'the no-script form default and visible recommendation must both be Roaming');
assert(html.includes('aria-describedby="rnode-interface-mode-hint"'),
    'the advanced mode control must expose its radio-specific guidance');
assert(html.includes('Show advanced interface controls and developer settings when available.'),
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
