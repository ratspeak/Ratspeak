'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var settingsSource = fs.readFileSync(
    path.join(__dirname, '..', 'static', 'js', 'settings.js'),
    'utf8'
);
var start = settingsSource.indexOf('var _settingsChannelHostingBound');
var end = settingsSource.indexOf('\nfunction readDeveloperModePreference', start);
assert(start >= 0 && end > start, 'missing channel hosting settings source');

var handlers = { off: {}, on: {} };
var groupState = {};
var group = {
    setAttribute: function(name, value) { groupState[name] = value; }
};
function radio(name, checked) {
    return {
        checked: checked,
        disabled: false,
        addEventListener: function(type, handler) { handlers[name][type] = handler; },
        closest: function() { return group; }
    };
}
var off = radio('off', true);
var on = radio('on', false);
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
            if (id === 'settings-channel-hosting-off') return off;
            if (id === 'settings-channel-hosting-on') return on;
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
    assert.strictEqual(typeof handlers.on.change, 'function',
        'entering Settings must bind the ON radio to backend state');

    off.checked = false;
    on.checked = true;
    handlers.on.change();
    await new Promise(function(resolve) { setImmediate(resolve); });

    assert.strictEqual(calls.length, 1);
    assert.strictEqual(calls[0].command, 'set_channel_hosting_enabled');
    assert.strictEqual(JSON.stringify(calls[0].args), JSON.stringify({ enabled: true }));
    assert.strictEqual(context.hostingEnabled(), true);
    assert.strictEqual(documentElement.dataset.channelHosting, 'on');
    assert.strictEqual(on.checked, true);
    assert.strictEqual(off.checked, false);
    assert(renders > 0, 'persisted ON state must rerender the Channels hosting card');
    assert.strictEqual(groupState['aria-busy'], 'false');

    console.log('settings channel hosting tests passed');
})().catch(function(error) {
    console.error(error);
    process.exitCode = 1;
});
