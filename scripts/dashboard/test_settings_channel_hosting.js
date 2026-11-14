'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var settingsSource = fs.readFileSync(
    path.join(__dirname, '..', '..', 'dashboard', 'static', 'js', 'settings.js'),
    'utf8'
);
var start = settingsSource.indexOf('var _settingsChannelHostingBound');
var end = settingsSource.indexOf('\nfunction syncActivityIdentityProtectionToggleState', start);
assert(start >= 0 && end > start, 'missing channel hosting settings source');

var handlers = {};
var busy = {};
var track = { classList: { toggle: function(name, on) { busy[name] = on; } } };
var toggle = {
    checked: false,
    disabled: false,
    attrs: {},
    setAttribute: function(name, value) { this.attrs[name] = value; },
    addEventListener: function(type, handler) { handlers[type] = handler; },
    closest: function() { return track; }
};
var desc = { textContent: '' };
var documentElement = { dataset: {} };
var calls = [];
var renders = 0;
var overview = {
    supported: true,
    hosting_enabled: true,
    created: false,
    settings: { enabled: false },
    status: { running: false }
};
var context = {
    window: {},
    document: {
        documentElement: documentElement,
        getElementById: function(id) {
            if (id === 'settings-channel-hosting-toggle') return toggle;
            if (id === 'settings-channel-hosting-desc') return desc;
            return null;
        }
    },
    RS: {
        invoke: function(command, args) {
            calls.push({ command: command, args: args });
            return Promise.resolve(overview);
        }
    },
    channelHubOverview: overview,
    channelHubRenderHome: function() { renders += 1; },
    _channelHubApplyOverview: function(next) {
        context.channelHubOverview = next;
        return next;
    }
};

vm.runInNewContext(
    settingsSource.slice(start, end) +
        '\nthis.initHosting = initChannelHostingToggle;' +
        '\nthis.hostingEnabled = window.ratspeakChannelHostingEnabled;',
    context,
    { filename: 'settings-channel-hosting.js' }
);

(async function() {
    context.initHosting();
    assert.strictEqual(typeof handlers.change, 'function',
        'entering Settings must bind the switch to backend state');

    toggle.checked = true;
    handlers.change();
    assert.strictEqual(busy['is-busy'], true, 'the switch spins while hosting turns on');
    assert.strictEqual(desc.textContent, 'Turning on channel hosting…');
    await new Promise(function(resolve) { setImmediate(resolve); });

    assert.strictEqual(calls.length, 1);
    assert.strictEqual(calls[0].command, 'set_channel_hosting_enabled');
    assert.strictEqual(JSON.stringify(calls[0].args), JSON.stringify({ enabled: true }));
    assert.strictEqual(context.hostingEnabled(), true);
    assert.strictEqual(documentElement.dataset.channelHosting, 'on');
    assert.strictEqual(toggle.checked, true);
    assert(renders > 0, 'persisted on state must rerender the Channels hosting card');
    assert.strictEqual(toggle.attrs['aria-busy'], 'false');
    assert.strictEqual(busy['is-busy'], false);
    assert.strictEqual(desc.textContent, 'Lets this device host channels.');

    console.log('settings channel hosting tests passed');
})().catch(function(error) {
    console.error(error);
    process.exitCode = 1;
});
