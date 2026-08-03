#!/usr/bin/env node
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var dashboardRoot = path.join(__dirname, '..');
var channelsSource = fs.readFileSync(
    path.join(dashboardRoot, 'static', 'js', 'channels.js'),
    'utf8'
);
var channelsCss = fs.readFileSync(
    path.join(dashboardRoot, 'static', 'css', '09-channels.css'),
    'utf8'
);
var indexSource = fs.readFileSync(path.join(dashboardRoot, 'index.html'), 'utf8');

function functionSource(name) {
    var start = channelsSource.indexOf('function ' + name + '(');
    assert.notStrictEqual(start, -1, name + ' must exist');
    var brace = channelsSource.indexOf('{', start);
    var depth = 0;
    for (var index = brace; index < channelsSource.length; index++) {
        if (channelsSource[index] === '{') depth += 1;
        if (channelsSource[index] === '}') {
            depth -= 1;
            if (depth === 0) return channelsSource.slice(start, index + 1);
        }
    }
    throw new Error('unterminated function ' + name);
}

var active = {
    hash: 'aa'.repeat(16),
    lxmf_hash: '11'.repeat(16)
};
var live = {
    identity_hash: 'aa'.repeat(16),
    hash: '11'.repeat(16)
};
var knownRemote = 'bb'.repeat(16);
var remoteLxmf = '22'.repeat(16);
var context = {
    Number: Number,
    String: String,
    activeIdentity: function() { return active; },
    lxmfIdentity: live,
    _channelsPeerForIdentity: function(identityHash) {
        return identityHash === knownRemote ? { hash: remoteLxmf } : null;
    },
    _channelsPeerLxmfAddress: function(peer) { return peer ? peer.hash : ''; },
    identityAvatar: function(seed, size) {
        return '<svg data-seed="' + seed + '" data-size="' + size + '"></svg>';
    },
    document: {
        createElement: function() {
            return { className: '', textContent: '' };
        }
    }
};
vm.createContext(context);
vm.runInContext(
    functionSource('_channelsNormalizeHistoryItem') + '\n' +
    functionSource('_channelsIdentityAvatarSeed') + '\n' +
    functionSource('_channelsPopulateIdentityAvatar'),
    context
);

var normalizedHistory = context._channelsNormalizeHistoryItem({
    event_id: 'event-1',
    source_hash: knownRemote,
    source_lxmf_hash: remoteLxmf
});
assert.strictEqual(normalizedHistory.source_hash, knownRemote);
assert.strictEqual(normalizedHistory.source_lxmf_hash, remoteLxmf,
    'persisted history must retain its canonical LXMF avatar seed');

assert.strictEqual(
    context._channelsIdentityAvatarSeed(active.hash, '', true),
    active.lxmf_hash,
    'the local channel identity must reuse the active LXMF avatar seed'
);
assert.strictEqual(
    context._channelsIdentityAvatarSeed(knownRemote, remoteLxmf, false),
    remoteLxmf,
    'a canonical remote LXMF destination must be used directly'
);
assert.strictEqual(
    context._channelsIdentityAvatarSeed(knownRemote, '', false),
    remoteLxmf,
    'a legacy snapshot may still reuse a discovered peer LXMF destination'
);
assert.strictEqual(
    context._channelsIdentityAvatarSeed('cc'.repeat(16), '', false),
    '',
    'an unidentified LXMF destination must use the neutral avatar'
);
assert.strictEqual(
    context._channelsIdentityAvatarSeed('dd'.repeat(16), '', true),
    '',
    'a mismatched local identity must not borrow the active identity avatar'
);
assert.strictEqual(
    context._channelsIdentityAvatarSeed('', '', false),
    '',
    'a nickname-only member must use the neutral avatar instead of a mutable seed'
);

var avatar = {
    innerHTML: '',
    attributes: {},
    children: [],
    setAttribute: function(name, value) { this.attributes[name] = value; },
    appendChild: function(child) { this.children.push(child); }
};
context._channelsPopulateIdentityAvatar(avatar, remoteLxmf, 40, 'Ada');
assert.strictEqual(avatar.attributes['aria-hidden'], 'true');
assert(avatar.innerHTML.includes('data-seed="' + remoteLxmf + '"'));
assert(avatar.innerHTML.includes('data-size="40"'));

context.identityAvatar = undefined;
var fallback = {
    innerHTML: '',
    attributes: {},
    children: [],
    setAttribute: function(name, value) { this.attributes[name] = value; },
    appendChild: function(child) { this.children.push(child); }
};
context._channelsPopulateIdentityAvatar(fallback, '', 40);
assert.strictEqual(fallback.children.length, 1);
assert.strictEqual(fallback.children[0].className, 'channel-avatar-fallback');
assert.strictEqual(fallback.children[0].textContent, '');

var transcriptStart = channelsSource.indexOf('function _channelsBuildTranscriptItem');
var transcriptEnd = channelsSource.indexOf('\nfunction _channelsMemberName', transcriptStart);
var transcriptSource = channelsSource.slice(transcriptStart, transcriptEnd);
assert(transcriptSource.includes("avatar.className = 'channel-event-avatar'"));
assert(transcriptSource.includes('_channelsIdentityAvatarSeed(item.source_hash, item.source_lxmf_hash, !!item.ours'));
assert(!transcriptSource.includes('channel-identity-marker'));
assert(!transcriptSource.includes('event.dataset.tone'));

var memberStart = channelsSource.indexOf('function _channelsRenderMembers');
var memberEnd = channelsSource.indexOf('\nfunction _channelsUpdateMobileMode', memberStart);
var memberSource = channelsSource.slice(memberStart, memberEnd);
assert(memberSource.includes("avatar.className = 'channel-member-avatar'"));
assert(memberSource.includes('_channelsIdentityAvatarSeed(member.identity_hash, member.lxmf_hash, !!member.is_self'));
assert(!memberSource.includes('channel-identity-marker'));
assert(!memberSource.includes('row.dataset.tone'));

var detailStart = channelsSource.indexOf('function _channelsRenderMemberDetail');
var detailEnd = channelsSource.indexOf('\nfunction _channelsShowMemberList', detailStart);
var detailSource = channelsSource.slice(detailStart, detailEnd);
assert(detailSource.includes("details.lxmfAddress || ''"));
assert(!detailSource.includes('details.lxmfAddress || details.identityHash'));

assert(channelsCss.includes('grid-template-areas:'));
assert(channelsCss.includes('"avatar author meta"'));
assert(channelsCss.includes('.channel-event-avatar'));
assert(channelsCss.includes('.channel-member-avatar'));
assert(!channelsCss.includes('.channel-identity-marker'));
assert(!channelsCss.includes('.channel-event[data-tone='));
assert(!channelsCss.includes('.channel-member-row[data-tone='));
assert(indexSource.includes('/static/style.css?v=1.0.39'));
assert(indexSource.includes('/static/js/channels.js?v=1.0.44'));

process.stdout.write('Channels avatar tests passed.\n');
