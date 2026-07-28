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

var statusContext = {};
vm.runInNewContext(
    sourceRange('_channelHubPlural', '_channelHubApplyOverview') +
        '\nthis.statusModel = _channelHubStatusModel;' +
        '\nthis.announceLabel = _channelHubAnnounceLabel;',
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

assert.strictEqual(statusContext.announceLabel(0), 'When started');
assert.strictEqual(statusContext.announceLabel(900), 'Every 15 min');
assert.strictEqual(statusContext.announceLabel(3600), 'Every hour');
assert.strictEqual(statusContext.announceLabel(21600), 'Every 6 hours');
assert.strictEqual(statusContext.announceLabel(86400), 'Every day');

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
    { checked: true },
    { checked: false }
);
assert.deepStrictEqual(JSON.parse(JSON.stringify(args)), {
    hub_name: 'Mountain hub',
    greeting: 'Welcome',
    announce_interval_secs: 900,
    resource_send: true,
    resource_accept: false
});
assert.strictEqual(configContext.settingsEqual({
    hub_name: 'Mountain hub',
    greeting: 'Welcome',
    announce_interval_secs: 900,
    resource_send_enabled: true,
    resource_accept_enabled: false
}, args), true);
assert.strictEqual(configContext.settingsEqual({
    hub_name: 'Mountain hub',
    greeting: 'Different',
    announce_interval_secs: 900,
    resource_send_enabled: true,
    resource_accept_enabled: false
}, args), false);

console.log('channel hub UI tests passed');
