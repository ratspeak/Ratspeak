'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var source = fs.readFileSync(
    path.join(__dirname, '..', 'static', 'js', 'channel_hub.js'),
    'utf8'
);

function sourceRange(firstName, lastName) {
    var start = source.indexOf('function ' + firstName);
    var end = source.indexOf('\nfunction ' + lastName, start);
    assert(start >= 0 && end > start, 'missing source range ' + firstName + '..' + lastName);
    return source.slice(start, end);
}

var statusContext = {
    window: {
        ratspeakChannelHostingEnabled: function() { return true; }
    }
};
vm.runInNewContext(
    sourceRange('_channelHubPlural', '_channelHubApplyOverview') +
        '\nthis.statusModel = _channelHubStatusModel;' +
        '\nthis.hostingEnabled = _channelHubHostingEnabled;',
    statusContext,
    { filename: 'channel-hub-status.js' }
);

var hosting = statusContext.statusModel({
    settings: { enabled: true },
    status: { running: true, welcomed_sessions: 2, registered_rooms: 3, registry_degraded: false }
});
assert.strictEqual(hosting.label, 'Hosting');
assert.strictEqual(hosting.detail, '2 people here · 3 channels');
assert.strictEqual(hosting.tone, 'online');
assert.strictEqual(hosting.action, 'stop');

var pendingSave = statusContext.statusModel({
    settings: { enabled: true },
    status: { running: true, welcomed_sessions: 1, registered_rooms: 1, registry_degraded: true }
});
assert.strictEqual(pendingSave.tone, 'warning');

var waiting = statusContext.statusModel({
    settings: { enabled: true },
    status: { running: false }
});
assert.strictEqual(waiting.label, 'Waiting for network');
assert.strictEqual(waiting.action, 'stop');

var stopped = statusContext.statusModel({
    settings: { enabled: false },
    status: { running: false }
});
assert.strictEqual(stopped.label, 'Not running');
assert.strictEqual(stopped.detail, 'Create a place for your community');
assert.strictEqual(stopped.action, 'start');

assert.strictEqual(statusContext.hostingEnabled({ hosting_enabled: false }), true);
assert.strictEqual(statusContext.hostingEnabled({ hosting_enabled: true }), true);
assert.strictEqual(statusContext.hostingEnabled({}), true);
statusContext.window.ratspeakChannelHostingEnabled = undefined;
assert.strictEqual(statusContext.hostingEnabled({ hosting_enabled: true }), false,
    'hosting defaults closed until Settings establishes an explicit preference');
statusContext.window.ratspeakChannelHostingEnabled = function() { return false; };
assert.strictEqual(statusContext.hostingEnabled({ hosting_enabled: true }), false,
    'a stale overview must not resurrect hosting after Settings is Off');

assert(source.indexOf(
    'var visible = _channelHubHostingEnabled(overview) && _channelHubHasOwnedHub(overview);'
) !== -1);
assert(source.indexOf(
    'if (overview.supported && _channelHubHostingEnabled(overview))'
) !== -1);

var configContext = {};
vm.runInNewContext(
    sourceRange('_channelHubConfigArgs', 'channelHubOpenManager') +
        '\nthis.configArgs = _channelHubConfigArgs;' +
        '\nthis.settingsEqual = _channelHubSettingsEqual;',
    configContext,
    { filename: 'channel-hub-config.js' }
);

var args = configContext.configArgs(
    { value: '  Mountain hub  ' },
    { value: ' Welcome ' },
    { value: '900' },
    { value: '21600' }
);
assert.deepStrictEqual(JSON.parse(JSON.stringify(args)), {
    hub_name: 'Mountain hub',
    greeting: 'Welcome',
    announce_interval_secs: 900,
    recent_activity_retention_secs: 21600
});
assert.strictEqual(configContext.settingsEqual({
    hub_name: 'Mountain hub',
    greeting: 'Welcome',
    announce_interval_secs: 900,
    recent_activity_retention_secs: 21600
}, args), true);
assert.strictEqual(configContext.settingsEqual({
    hub_name: 'Mountain hub',
    greeting: 'Different',
    announce_interval_secs: 900,
    recent_activity_retention_secs: 21600
}, args), false);

[
    "[900, 'Every 15 minutes']",
    "[1800, 'Every 30 minutes']",
    "[3600, 'Every hour']",
    "[43200, 'Every 12 hours']",
    "[86400, 'Every 24 hours']"
].forEach(function(option) {
    assert(source.indexOf(option) !== -1, 'missing announce choice ' + option);
});
assert(source.indexOf("[0, 'When started']") === -1);
assert(source.indexOf("[300, 'Every 5 min']") === -1);
assert(source.indexOf("[21600, 'Every 6 hours']") === -1);
assert(source.indexOf('At startup and on this schedule, so nearby people can find it') !== -1);
assert(source.indexOf('Large welcome messages') === -1);
assert(source.indexOf('Large room notices') === -1);
assert(source.indexOf('Operating limits') === -1);

console.log('channel hub UI tests passed');
