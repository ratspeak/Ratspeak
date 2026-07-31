#!/usr/bin/env node
// Deterministic regressions for the pull-only, read-only hub Admin Center.

'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var dashboardRoot = path.join(__dirname, '..');
var source = fs.readFileSync(
    path.join(dashboardRoot, 'static', 'js', 'channel_hub.js'),
    'utf8'
);
var channelsCss = fs.readFileSync(
    path.join(dashboardRoot, 'static', 'css', '09-channels.css'),
    'utf8'
);
var responsiveCss = fs.readFileSync(
    path.join(dashboardRoot, 'static', 'css', '13-responsive.css'),
    'utf8'
);

function sourceRange(firstName, lastName) {
    var start = source.indexOf('function ' + firstName);
    var end = source.indexOf('\nfunction ' + lastName, start);
    assert(start >= 0 && end > start, 'missing source range ' + firstName + '..' + lastName);
    return source.slice(start, end);
}

function deferred() {
    var resolve;
    var reject;
    var promise = new Promise(function(ok, fail) {
        resolve = ok;
        reject = fail;
    });
    return { promise: promise, resolve: resolve, reject: reject };
}

function FakeElement(tagName) {
    this.tagName = String(tagName || 'div').toUpperCase();
    this.children = [];
    this.dataset = {};
    this.attributes = {};
    this.listeners = {};
    this.className = '';
    this.hidden = false;
    this.disabled = false;
    this._text = '';
}

Object.defineProperty(FakeElement.prototype, 'textContent', {
    get: function() { return this._text; },
    set: function(value) {
        this._text = String(value == null ? '' : value);
        if (this._text === '') this.children = [];
    }
});

Object.defineProperty(FakeElement.prototype, 'innerHTML', {
    get: function() { return ''; },
    set: function() {
        throw new Error('Admin Center network data must never use innerHTML');
    }
});

FakeElement.prototype.appendChild = function(child) {
    this.children.push(child);
    return child;
};

FakeElement.prototype.setAttribute = function(name, value) {
    this.attributes[name] = String(value);
};

FakeElement.prototype.addEventListener = function(name, handler) {
    this.listeners[name] = handler;
};

function descendants(root) {
    var result = [];
    (root.children || []).forEach(function(child) {
        result.push(child);
        result = result.concat(descendants(child));
    });
    return result;
}

function textTree(root) {
    return [root.textContent].concat((root.children || []).map(textTree)).join(' ');
}

function adminSnapshot(overrides) {
    var snapshot = {
        model_version: 1,
        running: true,
        generated_at_ms: 1785456000000,
        uptime_secs: 3720,
        pending_sessions: 1,
        registry_degraded: false,
        rooms: [{
            name: 'field',
            topic: 'Field coordination',
            registered: true,
            live_member_count: 1,
            live_session_count: 2,
            modes: {
                invite_only: true,
                join_key_configured: true,
                moderated: true,
                no_outside_messages: true,
                private: true,
                topic_operators_only: true
            },
            operators: ['aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'],
            voiced: [],
            bans: [],
            invitations: [{
                identity_hash: 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                expires_at_ms: 1785456900000
            }],
            last_used_ms: 1785455999000,
            save_pending: false
        }],
        people: [{
            identity_hash: 'cccccccccccccccccccccccccccccccc',
            nickname: 'Field Rat',
            session_count: 2,
            welcomed_session_count: 1,
            connected_for_secs: 130,
            rooms: ['field'],
            server_operator: true,
            room_operator_in: ['field'],
            voiced_in: []
        }],
        server_operators: ['cccccccccccccccccccccccccccccccc'],
        hub_bans: ['dddddddddddddddddddddddddddddddd'],
        stats: {
            joins: 2,
            parts: 1,
            messages_forwarded: 3,
            notices_forwarded: 1,
            actions_forwarded: 1,
            direct_notices: 0,
            pings_out: 4,
            pongs_in: 4,
            rate_limited: 1,
            bad_packets: 1,
            duplicates: 1,
            resources_received: 1,
            resource_bytes_received: 2048,
            resources_rejected: 0,
            oversize: 1
        },
        limits: {
            max_registered_rooms: 256,
            max_rooms_per_session: 32,
            max_message_body_bytes: 350,
            rate_messages_per_minute: 240,
            invite_timeout_secs: 900,
            rejoin_grace_secs: 120,
            max_resource_notice_bytes: 4096,
            max_resource_bytes: 262144
        },
        evidence_policy: {
            retention_secs: 900,
            max_events: 128,
            max_estimated_bytes: 65536,
            max_excerpt_bytes: 256,
            persistent: false
        },
        evidence: [{
            sequence: '9',
            observed_at_ms: 1785456000000,
            kind: 'message',
            action: null,
            count: null,
            room: 'field',
            source_identity_hash: 'cccccccccccccccccccccccccccccccc',
            source_nickname: 'Field Rat',
            target_identity_hash: null,
            excerpt: 'Weather moved east.'
        }],
        evidence_evicted: 2
    };
    Object.keys(overrides || {}).forEach(function(key) {
        snapshot[key] = overrides[key];
    });
    return snapshot;
}

var helperSource = sourceRange('_channelHubAdminNode', 'channelHubOpenManager');
assert.strictEqual(helperSource.indexOf('innerHTML'), -1,
    'all admin projections must render network data through DOM text APIs');
assert.strictEqual(helperSource.indexOf('localStorage'), -1);
assert.strictEqual(helperSource.indexOf('sessionStorage'), -1);
assert.strictEqual(helperSource.indexOf('setInterval'), -1);

var context = {
    document: {
        createElement: function(tagName) { return new FakeElement(tagName); }
    },
    RS: {
        copyText: function() { return Promise.resolve(true); }
    },
    _channelHubPlural: function(count, singular, plural) {
        return count + ' ' + (count === 1 ? singular : (plural || singular + 's'));
    },
    Promise: Promise,
    Date: Date,
    Number: Number,
    String: String,
    Array: Array,
    Object: Object
};

vm.runInNewContext(
    helperSource +
        '\nthis.modeLabels = _channelHubAdminModeLabels;' +
        '\nthis.evidenceModel = _channelHubAdminEvidenceModel;' +
        '\nthis.renderOverview = _channelHubRenderAdminOverview;' +
        '\nthis.renderChannels = _channelHubRenderAdminChannels;' +
        '\nthis.renderPeople = _channelHubRenderAdminPeople;' +
        '\nthis.renderAccess = _channelHubRenderAdminAccess;' +
        '\nthis.renderActivity = _channelHubRenderAdminActivity;' +
        '\nthis.renderLimits = _channelHubRenderAdminLimits;',
    context,
    { filename: 'channel-hub-admin-render.js' }
);

assert.deepStrictEqual(
    Array.from(context.modeLabels(adminSnapshot().rooms[0])),
    [
        'Registered',
        'Private',
        'Invite only',
        'Join key',
        'Moderated',
        'Members post',
        'Operator topics'
    ]
);
assert.deepStrictEqual(
    Array.from(context.modeLabels({
        registered: false,
        modes: {}
    })),
    ['Session-only', 'Open']
);

var moderation = context.evidenceModel({
    kind: 'moderation',
    action: 'kick',
    room: 'field',
    source_nickname: 'Operator',
    target_identity_hash: 'eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee'
});
assert.strictEqual(moderation.title, 'Operator removed someone from the channel');
assert(moderation.detail.indexOf('#field') !== -1);
assert(moderation.detail.indexOf('eeeeeeee') !== -1);

var malicious = '<img src=x onerror=globalThis.pwned=true>\u202e literal';
var activityData = adminSnapshot({
    evidence: [{
        sequence: '10',
        observed_at_ms: 1785456000000,
        kind: 'message',
        action: null,
        count: null,
        room: '<room>',
        source_identity_hash: 'ffffffffffffffffffffffffffffffff',
        source_nickname: '<script>operator()</script>',
        target_identity_hash: null,
        excerpt: malicious
    }]
});
var activityRoot = new FakeElement('section');
context.renderActivity(activityRoot, activityData, function() {});
var activityText = textTree(activityRoot);
assert(activityText.indexOf('Recent context, not a transcript') !== -1);
assert(activityText.indexOf('Memory-only and incomplete') !== -1);
assert(activityText.indexOf('display-sanitized and capped at 256 B') !== -1);
assert(activityText.indexOf(malicious) !== -1,
    'hostile-looking evidence must remain visible as literal text');
assert.strictEqual(descendants(activityRoot).filter(function(node) {
    return node.tagName === 'IMG' || node.tagName === 'SCRIPT';
}).length, 0);

var stoppedActivity = new FakeElement('section');
context.renderActivity(stoppedActivity, adminSnapshot({
    running: false,
    people: [],
    evidence: []
}), function() {});
assert(textTree(stoppedActivity).indexOf('No activity while stopped') !== -1);
assert(textTree(stoppedActivity).indexOf('never persisted') !== -1);

var overviewRoot = new FakeElement('section');
context.renderOverview(overviewRoot, adminSnapshot(), function() {});
var overviewText = textTree(overviewRoot);
assert(overviewText.indexOf('Unique identities') !== -1);
assert(overviewText.indexOf('Live sessions') !== -1);
assert(overviewText.indexOf('Policy is durable. Conversation traffic is not.') !== -1);

var channelsRoot = new FakeElement('section');
context.renderChannels(channelsRoot, adminSnapshot(), function() {});
var channelsText = textTree(channelsRoot);
assert(channelsText.indexOf('#field') !== -1);
assert(channelsText.indexOf('Private') !== -1);
assert(channelsText.indexOf('2 sessions') !== -1);

var peopleRoot = new FakeElement('section');
context.renderPeople(peopleRoot, adminSnapshot(), function() {});
var peopleText = textTree(peopleRoot);
assert(peopleText.indexOf('Field Rat') !== -1);
assert(peopleText.indexOf('Hub operator') !== -1);
assert(peopleText.indexOf('Handshake pending') !== -1);

var accessRoot = new FakeElement('section');
context.renderAccess(accessRoot, adminSnapshot(), function() {});
var accessText = textTree(accessRoot);
assert(accessText.indexOf('Hub operators') !== -1);
assert(accessText.indexOf('Hub bans') !== -1);
assert(accessText.indexOf('Invitations') !== -1);

var limitsRoot = new FakeElement('section');
context.renderLimits(limitsRoot, adminSnapshot());
var limitsText = textTree(limitsRoot);
assert(limitsText.indexOf('Operating limits') !== -1);
assert(limitsText.indexOf('256 KiB') !== -1);
assert(limitsText.indexOf('not editable in this release') !== -1);

var managerSource = sourceRange('channelHubOpenManager', 'channelHubOpenOwnHub');
assert(managerSource.indexOf("RS.invoke('api_channel_hub_admin')") !== -1);
assert(managerSource.indexOf('Number(nextAdmin.model_version) !== 1') !== -1);
assert(managerSource.indexOf('nextAdmin.evidence_policy.persistent !== false') !== -1);
assert(managerSource.indexOf('request !== adminRequest') !== -1);
assert(managerSource.indexOf('_channelHubManagerSequence !== sequence') !== -1);
assert(managerSource.indexOf("setActiveTab('overview'") !== -1);
assert(managerSource.indexOf("id: 'activity', label: 'Activity'") !== -1);
assert(managerSource.indexOf('setInterval') === -1,
    'owner evidence must never gain background polling');
assert(managerSource.indexOf('localStorage') === -1,
    'owner evidence must never enter browser persistence');
assert(source.indexOf('var _channelHubManagerDismiss = null;') !== -1);
assert(source.indexOf("RS.listen('lxmf_identity'") !== -1);
assert(source.indexOf('dismissManager();') !== -1,
    'identity replacement must close an owner view for the previous identity');

assert(channelsCss.indexOf('.channel-host-admin-tabs') !== -1);
assert(channelsCss.indexOf('.channel-host-admin-timeline') !== -1);
assert(channelsCss.indexOf('unicode-bidi: plaintext') !== -1);
assert(responsiveCss.indexOf('.channel-host-admin-metrics') !== -1);
assert(responsiveCss.indexOf('grid-template-columns: repeat(2, minmax(0, 1fr))') !== -1);

async function testIdentityGenerationFence() {
    var oldRequest = deferred();
    var newRequest = deferred();
    var requests = [oldRequest, newRequest];
    var applied = [];
    var loadContext = {
        channelHubOverview: null,
        _channelHubOverviewLoadedAt: 0,
        _channelHubOverviewPromise: null,
        _channelHubIdentityGeneration: 7,
        _channelHubApplyOverview: function(overview) {
            applied.push(overview);
            loadContext.channelHubOverview = overview;
            return overview;
        },
        RS: {
            invoke: function(command) {
                assert.strictEqual(command, 'api_channel_hub');
                return requests.shift().promise;
            }
        },
        Date: Date,
        Promise: Promise
    };
    vm.runInNewContext(
        sourceRange('channelHubLoad', '_channelHubIcon'),
        loadContext,
        { filename: 'channel-hub-identity-load.js' }
    );

    var stale = loadContext.channelHubLoad(true);
    loadContext._channelHubIdentityGeneration += 1;
    loadContext._channelHubOverviewPromise = null;
    var current = loadContext.channelHubLoad(true);

    oldRequest.resolve({ destination_hash: 'old-identity-hub' });
    assert.strictEqual(await stale, null,
        'a response captured before identity replacement must be discarded');
    assert.deepStrictEqual(applied, []);

    newRequest.resolve({ destination_hash: 'current-identity-hub' });
    assert.strictEqual((await current).destination_hash, 'current-identity-hub');
    assert.deepStrictEqual(applied, [{ destination_hash: 'current-identity-hub' }]);
    await Promise.resolve();
    assert.strictEqual(loadContext._channelHubOverviewPromise, null);
}

testIdentityGenerationFence().then(function() {
    console.log('channel hub Admin Center tests passed');
}).catch(function(error) {
    console.error(error);
    process.exitCode = 1;
});
