#!/usr/bin/env node
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var source = fs.readFileSync(
    path.join(__dirname, '..', 'static', 'js', 'peers_cache.js'),
    'utf8'
);

var notifications = 0;
var context = {
    window: { RS: { diag: function() {} } },
    RS: { invoke: function() { return Promise.resolve([]); } },
    console: console,
    setInterval: function() { return 1; },
    Date: Date,
    prettyTime: function(seconds) { return String(seconds); },
    lastStats: null,
    lxmfContacts: [],
    lxmfConversations: []
};
vm.runInNewContext(source, context, { filename: 'peers_cache.js' });

var cache = context.PeersCache;
assert(cache, 'PeersCache must be exported');
cache.subscribe(function() { notifications += 1; });

var delivery = ['lxmf.delivery'];
cache.replace([
    { hash: '01', display_name: '!deadbeef', services: delivery },
    { hash: '02', display_name: 'deadbeef', services: delivery },
    { hash: '03', display_name: 'Meshtastic bridge', services: delivery },
    { hash: '04', display_name: 'username', services: delivery },
    { hash: '05', display_name: 'cafegaze', services: delivery },
    { hash: '06', display_name: 'not-actionable', services: ['other.service'] },
    { hash: '07', display_name: 'face1234', services: delivery, is_contact: true },
    { hash: '08', display_name: 'feed1234', services: ['lxmf.delivery', 'ratspeak.client'] },
    { hash: '09', display_name: 'beef1234', services: ['lxmf.delivery', 'lxst.telephony'] },
    { hash: '10', display_name: 'badc0ffe', services: delivery, profile_status: 'Available' },
    { hash: '11', display_name: 'decafbad', services: delivery }
]);

context.lxmfConversations = [{ hash: '11' }];
cache.visibilityContextChanged();

assert.strictEqual(cache.isSuppressedPeerDisplayName('!deadbeef'), true);
assert.strictEqual(cache.isSuppressedPeerDisplayName('deadbeef'), true,
    'bridge-stripped eight-hex ids must match');
assert.strictEqual(cache.isSuppressedPeerDisplayName('Meshtastic bridge'), true);
assert.strictEqual(cache.isSuppressedPeerDisplayName('username'), false,
    'normal eight-character usernames must remain visible');
assert.strictEqual(cache.isSuppressedPeerDisplayName('cafegaze'), false,
    'eight-character strings containing non-hex characters must remain visible');
assert.strictEqual(cache.isSuppressedPeerDisplayName('li5ab0s5'), false,
    'human-looking eight-character names are not hexadecimal bridge ids');

assert.deepStrictEqual(
    Array.from(cache.getAll(), function(peer) { return peer.hash; }).sort(),
    ['02', '04', '05', '07', '08', '09', '10', '11'],
    'explicit bridge ids are hidden while an isolated bare-hex name and relationship evidence remain visible'
);

cache.replace([
    { hash: '20', display_name: 'deadbeef', services: delivery },
    { hash: '21', display_name: 'cafe1234', services: delivery },
    { hash: '22', display_name: 'regular-user', services: delivery }
]);
assert.deepStrictEqual(
    Array.from(cache.getAll(), function(peer) { return peer.hash; }).sort(),
    ['20', '21', '22'],
    'one or two ambiguous bare-hex names remain visible'
);

cache.applyUpdated({ hash: '23', display_name: 'abcdef12', services: delivery });
assert.deepStrictEqual(
    Array.from(cache.getAll(), function(peer) { return peer.hash; }).sort(),
    ['22'],
    'three otherwise-anonymous bare-hex peers form a suppressible bridge-noise cluster'
);

cache.applyUpdated({ hash: '20', is_contact: true });
assert.deepStrictEqual(
    Array.from(cache.getAll(), function(peer) { return peer.hash; }).sort(),
    ['20', '21', '22', '23'],
    'saving a peer overrides suppression and dissolves a cluster with fewer than three unknown peers'
);

cache.applyUpdated({ hash: '25', display_name: '1234abcd', services: delivery });
assert.deepStrictEqual(
    Array.from(cache.getAll(), function(peer) { return peer.hash; }).sort(),
    ['20', '22'],
    'known peers remain visible when a third unknown bare-hex peer restores the cluster signal'
);

var beforeToggleNotifications = notifications;
cache.setHideKnownSpamPeers(false);
assert.strictEqual(cache.hideKnownSpamPeersEnabled(), false);
assert(notifications > beforeToggleNotifications,
    'changing the preference must invalidate and notify all peer views');
assert.deepStrictEqual(
    Array.from(cache.getAll(), function(peer) { return peer.hash; }).sort(),
    ['20', '21', '22', '23', '25'],
    'turning the preference off restores name-filtered peers only'
);

cache.applyUpdated({ hash: '24', display_name: 'not-actionable', services: ['other.service'] });
assert.strictEqual(cache.get('24'), null,
    'protocol/service eligibility remains independent of the display preference');

cache.setHideKnownSpamPeers(true);
assert.strictEqual(cache.get('21'), null);
assert.strictEqual(cache.get('20').display_name, 'deadbeef');

context.lxmfConversations = [{ hash: '21' }];
var beforeConversationNotifications = notifications;
cache.visibilityContextChanged();
assert(notifications > beforeConversationNotifications,
    'conversation updates must invalidate every peer view');
assert.strictEqual(cache.get('21').display_name, 'cafe1234',
    'a prior conversation restores an otherwise ambiguous peer');

console.log('peer spam visibility tests passed');
