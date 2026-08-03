#!/usr/bin/env node
// Deterministic typed-model and source coverage for authenticated hub identity,
// welcome provenance, capabilities, limits, and public-directory presentation.

'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var root = path.join(__dirname, '..');
var channelsSource = fs.readFileSync(
    path.join(root, 'static', 'js', 'channels.js'),
    'utf8'
);
var hubSource = fs.readFileSync(
    path.join(root, 'static', 'js', 'channel_hub.js'),
    'utf8'
);
var cssSource = fs.readFileSync(
    path.join(root, 'static', 'css', '09-channels.css'),
    'utf8'
);
var responsiveSource = fs.readFileSync(
    path.join(root, 'static', 'css', '13-responsive.css'),
    'utf8'
);

function sourceRange(startName, endName) {
    var start = channelsSource.indexOf('function ' + startName);
    var end = channelsSource.indexOf('\nfunction ' + endName, start);
    assert(start !== -1 && end !== -1, startName + ' source range must exist');
    return channelsSource.slice(start, end);
}

var modelSource = sourceRange(
    '_channelsCurrentHubObserved',
    '_channelsConnectCommandBlocked'
);
var context = {
    Array: Array,
    Math: Math,
    Number: Number,
    String: String,
    channelsSnapshot: {}
};
vm.runInNewContext(modelSource, context, {
    filename: 'channels-hub-profile-model.js'
});

var HUB = '00112233445566778899aabbccddeeff';
var IDENTITY = 'ffeeddccbbaa99887766554433221100';
var greeting = {
    text: 'Read the field rules before transmitting.',
    received_at_ms: 1_900_000_000_000,
    source_hash: IDENTITY,
    delivery: 'resource',
    completeness: 'complete'
};
context.channelsSnapshot = {
    protocol_version: '0.1.3',
    phase: 'active',
    nickname: 'Field Rat',
    selected_hub_destination: HUB,
    hub: {
        destination_hash: HUB,
        name: 'Legacy root projection'
    },
    directory: { phase: 'idle', rooms: [] },
    hub_greeting: null,
    hubs: [{
        destination_hash: HUB,
        observed: {
            phase: 'active',
            nickname: 'Field Rat',
            hub: {
                destination_hash: HUB,
                identity_hash: IDENTITY,
                announced_name: 'Ridge Relay',
                name: 'Ridge Operations',
                version: '0.3.2',
                hops: 2,
                link_mdu: 383,
                connected_at_ms: 1_900_000_000_000,
                capabilities: {
                    actions: true,
                    direct_notices: true,
                    resource_envelopes: true,
                    rejoin_grace: false
                },
                limits: {
                    max_nick_bytes: 32,
                    max_room_name_bytes: 64,
                    max_message_body_bytes: 350,
                    max_rooms_per_session: 32,
                    rate_messages_per_minute: 240
                }
            },
            directory: {
                phase: 'ready',
                rooms: [{ name: 'field' }, { name: 'ops' }],
                complete: false,
                omitted_count: 3,
                refreshed_at_ms: 1_900_000_000_500
            },
            greeting: greeting
        }
    }]
};

var profile = context._channelsHubProfileModel();
assert.strictEqual(profile.display_name, 'Ridge Operations');
assert.strictEqual(profile.authenticated_name, 'Ridge Operations');
assert.strictEqual(profile.announced_name, 'Ridge Relay');
assert.strictEqual(profile.name_mismatch, true);
assert.strictEqual(profile.destination_hash, HUB);
assert.strictEqual(profile.identity_hash, IDENTITY);
assert.strictEqual(profile.authenticated_session, true);
assert.strictEqual(profile.hops, 2);
assert.strictEqual(profile.link_mdu, 383);
assert.strictEqual(profile.greeting, greeting,
    'the hub-keyed typed greeting must outrank the legacy root projection');
assert.strictEqual(profile.directory.count, 2);
assert.strictEqual(profile.directory.complete, false);
assert.strictEqual(
    profile.directory.summary,
    '2 public channels shown \u00b7 3 more omitted by the hub'
);
assert.strictEqual(profile.capabilities.resource_envelopes, true);
assert.strictEqual(profile.capabilities.rejoin_grace, false);
assert.strictEqual(profile.limits.max_message_body_bytes, 350);
assert.strictEqual(profile.limits.rate_messages_per_minute, 240);

context.channelsSnapshot = {
    protocol_version: '0.1.3',
    phase: 'stale',
    hubs: [],
    hub: {
        destination_hash: HUB,
        identity_hash: IDENTITY,
        announced_name: 'Same Name',
        name: 'same name',
        capabilities: {},
        limits: {
            max_nick_bytes: null,
            max_message_body_bytes: undefined
        }
    },
    directory: {
        phase: 'idle',
        rooms: [],
        complete: false,
        omitted_count: null
    },
    hub_greeting: {
        text: 'Short packet greeting',
        delivery: 'notice',
        completeness: 'unframed'
    }
};
profile = context._channelsHubProfileModel();
assert.strictEqual(profile.name_mismatch, false,
    'display-only name comparison should be case insensitive');
assert.strictEqual(profile.authenticated_session, false,
    'an identity remembered from a stale projection is not a current Link claim');
assert.strictEqual(profile.limits.max_nick_bytes, null,
    'a missing native limit must not be coerced into a zero-byte limit');
assert.strictEqual(profile.limits.max_message_body_bytes, null);
assert.strictEqual(
    profile.directory.summary,
    'Public channels have not been requested for this Link.'
);
assert.strictEqual(profile.greeting.delivery, 'notice');
assert.strictEqual(profile.greeting.completeness, 'unframed');

assert(modelSource.indexOf('JSON.parse') === -1);
assert(modelSource.indexOf('cbor') === -1);
assert(modelSource.indexOf('.body') === -1,
    'JavaScript must consume typed native fields instead of protocol bodies');
assert(channelsSource.indexOf('Hub welcome') !== -1);
assert(channelsSource.indexOf('NOTICE delivery') === -1);
assert(channelsSource.indexOf('Complete bounded transfer') === -1);
assert(channelsSource.indexOf("home.className = 'channel-hub-home'") !== -1);
assert(channelsSource.indexOf('Authenticated channel hub') !== -1);
assert(channelsSource.indexOf('Name differs from the recent announce') !== -1);
assert(channelsSource.indexOf('Private or secret channels may be intentionally hidden') !== -1);
assert(channelsSource.indexOf('Advertised by this hub in the authenticated WELCOME.') !== -1);
assert(channelsSource.indexOf("secondary.dataset.channelAction = 'hub-info'") !== -1);
assert(channelsSource.indexOf("action === 'hub-info'") !== -1);
assert(hubSource.indexOf("'Welcome & guidance'") !== -1);
assert(hubSource.indexOf('Use for rules and where to begin') !== -1);
assert(cssSource.indexOf('.channel-hub-profile-capabilities') !== -1);
assert(cssSource.indexOf('.channel-hub-greeting-delivery') === -1);
assert(cssSource.indexOf('.channel-hub-home') !== -1);
assert(responsiveSource.indexOf('.channel-hub-profile-capabilities') !== -1);

console.log('channel hub profile tests passed');
