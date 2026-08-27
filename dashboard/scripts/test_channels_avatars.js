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
var componentsCss = fs.readFileSync(
    path.join(dashboardRoot, 'static', 'css', '07-components.css'),
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
    functionSource('_channelsAvatarFallbackLabel') + '\n' +
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
context._channelsPopulateIdentityAvatar(fallback, '', 40, 'Ada');
assert.strictEqual(fallback.children.length, 1);
assert.strictEqual(fallback.children[0].className, 'channel-avatar-fallback');
assert.strictEqual(fallback.children[0].textContent, 'A');
assert.strictEqual(fallback.innerHTML, '',
    'a nickname must never be fed into the hash-derived avatar generator');

var memberNameContext = {
    String: String,
    _channelsShortHash: function(hash) { return hash ? 'short-hash' : ''; },
    _channelsMemberDetails: function(member) {
        return { knownName: member.known_name || '' };
    }
};
vm.createContext(memberNameContext);
vm.runInContext(functionSource('_channelsMemberListName'), memberNameContext);
assert.strictEqual(memberNameContext._channelsMemberListName({
    identity_hash: knownRemote,
    nickname: null,
    known_name: 'Runr01'
}), 'Runr01', 'a seeded identity must use its already-known peer name immediately');
assert.strictEqual(memberNameContext._channelsMemberListName({
    identity_hash: knownRemote,
    nickname: 'Runr',
    known_name: 'Runr01'
}), 'Runr', 'a channel-supplied nickname must supersede the prior known peer name');
assert.strictEqual(memberNameContext._channelsMemberListName({
    identity_hash: knownRemote,
    nickname: null,
    known_name: ''
}), 'short-hash', 'an unknown seeded identity must retain the bounded hash fallback');

var transcriptStart = channelsSource.indexOf('function _channelsBuildTranscriptItem');
var transcriptEnd = channelsSource.indexOf('\nfunction _channelsMemberName', transcriptStart);
var transcriptSource = channelsSource.slice(transcriptStart, transcriptEnd);
assert(transcriptSource.includes("avatar.className = 'channel-event-avatar'"));
assert(transcriptSource.includes('_channelsIdentityAvatarSeed(item.source_hash, item.source_lxmf_hash, !!item.ours'));
assert(!transcriptSource.includes('channel-identity-marker'));
assert(!transcriptSource.includes('event.dataset.tone'));

var memberStart = channelsSource.indexOf('function _channelsBuildMemberRow');
var memberEnd = channelsSource.indexOf('\nfunction _channelsUpdateComposer', memberStart);
var memberSource = channelsSource.slice(memberStart, memberEnd);
assert(memberSource.includes("avatar.className = 'channel-member-avatar'"));
assert(memberSource.includes('var nameText = _channelsMemberListName(member);'));
assert(memberSource.includes('_channelsIdentityAvatarSeed(member.identity_hash, member.lxmf_hash, !!member.is_self'));
assert(!memberSource.includes('channel-identity-marker'));
assert(!memberSource.includes('row.dataset.tone'));
assert(memberSource.includes("_channelsAppendMemberGroup(list, 'Recently visible'"));
assert(memberSource.includes("_channelsAppendMemberGroup(list, 'Seen here'"));
assert(memberSource.includes("model.visible.length + ' visible'"),
    'historical people must never inflate the live visible count');

var rosterNow = 5_000;
var returnerHash = 'cc'.repeat(16);
var exitedHash = 'dd'.repeat(16);
var historyOnlyHash = 'ee'.repeat(16);
var rosterEntry = {
    participants: [
        { identity_hash: knownRemote, nickname: 'Ada', last_seen_at_ms: 4_900, _seen: true },
        { identity_hash: returnerHash, nickname: 'Returner', last_seen_at_ms: 4_000, _seen: true },
        { identity_hash: exitedHash, nickname: 'Exited', last_seen_at_ms: 3_000, _seen: true },
        { identity_hash: null, nickname: 'Guest', last_seen_at_ms: 4_600, _seen: true },
        { identity_hash: historyOnlyHash, nickname: 'Dana', last_seen_at_ms: 4_500, _seen: true },
        { identity_hash: null, nickname: 'Dana', last_seen_at_ms: 4_400, _seen: true }
    ],
    participants_omitted: 3
};
var observedRoom = { members: {} };
observedRoom.members['identity:' + returnerHash] = {
    member: { identity_hash: returnerHash, nickname: 'Returner', is_self: false },
    last_visible_at_ms: 4_800,
    continuity_until_ms: 6_000
};
observedRoom.members['identity:' + exitedHash] = {
    member: { identity_hash: exitedHash, nickname: 'Exited', is_self: false },
    last_visible_at_ms: 4_700,
    continuity_until_ms: 0
};
var rosterContext = {
    Array: Array,
    Date: { now: function() { return rosterNow; } },
    Number: Number,
    Object: Object,
    String: String,
    _channelsObservedMembersByRoom: { room: observedRoom },
    _channelsHistoryContext: function() { return { key: 'room' }; },
    _channelsHistoryEntry: function() { return rosterEntry; },
    _channelsIsBlockedMember: function(member) { return !!(member && member._blocked); }
};
vm.createContext(rosterContext);
vm.runInContext(
    functionSource('_channelsMemberKey') + '\n' +
    functionSource('_channelsMemberRosterModel'),
    rosterContext
);
var rosterRoom = {
    members_complete: false,
    members: [
        { identity_hash: active.hash, nickname: 'Bob', is_self: true },
        { identity_hash: knownRemote, nickname: 'Ada', is_self: false },
        { identity_hash: 'ff'.repeat(16), nickname: 'Blocked', is_self: false, _blocked: true }
    ]
};
var roster = rosterContext._channelsMemberRosterModel(rosterRoom);
assert.strictEqual(roster.visible.length, 2);
assert(!roster.visible.some(function(member) { return member.nickname === 'Blocked'; }),
    'blocked peers must not leak into the visible member roster');
assert.deepStrictEqual(Array.from(roster.continuity, function(member) { return member.nickname; }),
    ['Returner'], 'a short reconnect should keep prior live peers near-normal while reconfirming');
assert.deepStrictEqual(Array.from(roster.seen, function(member) { return member.nickname; }),
    ['Exited', 'Guest', 'Dana'],
    'seen history must prefer an identified peer over an unresolved duplicate nickname');
assert.strictEqual(roster.omitted, 3);
rosterRoom.members_complete = true;
var completeRoster = rosterContext._channelsMemberRosterModel(rosterRoom);
assert.strictEqual(completeRoster.continuity.length, 0,
    'a complete hub roster should resolve reconnect uncertainty immediately');
assert.strictEqual(completeRoster.seen[0].nickname, 'Returner');
assert(!completeRoster.seen.some(function(member) { return member.is_self; }),
    'the local identity must never be duplicated in Seen here');

var applySnapshotSource = functionSource('channelsApplySnapshot');
assert(applySnapshotSource.includes("snapshot.phase === 'reconnecting'") &&
        applySnapshotSource.includes("oldPhase === 'active' || oldPhase === 'stale'"),
    'continuity starts only when a live same-hub session enters recovery');
assert(channelsSource.includes('var CHANNEL_MEMBER_CONTINUITY_MS = 60 * 1000;'));

var detailStart = channelsSource.indexOf('function _channelsAppendMemberDetail');
var detailEnd = channelsSource.indexOf('\nfunction _channelsFocusMemberRow', detailStart);
var detailSource = channelsSource.slice(detailStart, detailEnd);
assert(detailSource.includes("details.lxmfAddress || ''"));
assert(!detailSource.includes('details.lxmfAddress || details.identityHash'));
assert(channelsSource.includes("built.sheet.classList.add('channel-member-profile-sheet')"));
assert(channelsSource.includes("title: 'Member details'"));
assert(channelsSource.includes('avatarSize: 64'));
assert(channelsSource.includes('if (_channelsCompact()) {'));
assert(channelsSource.includes('PeersCache.subscribe(_channelsRefreshMemberNamesFromPeers);'),
    'peer-cache hydration must repaint a visible channel roster without requiring a member action');

assert(channelsCss.includes('grid-template-areas:'));
assert(channelsCss.includes('"avatar author meta"'));
assert(channelsCss.includes('.channel-event-avatar'));
assert(channelsCss.includes('.channel-member-avatar'));
assert(channelsCss.includes('.channel-member-row.reconfirming'));
assert(channelsCss.includes('.channel-member-row.seen'));
assert(channelsCss.includes('.channel-member-group-label'));
assert(channelsCss.includes('.channel-avatar-fallback'));
assert(!channelsCss.includes('.channel-identity-marker'));
assert(!channelsCss.includes('.channel-event[data-tone='));
assert(!channelsCss.includes('.channel-member-row[data-tone='));
assert(channelsCss.includes('.channel-members-pane.showing-detail .channel-members-heading'));
assert(channelsCss.includes('.channel-member-profile-sheet .bottom-sheet-body'));
assert(indexSource.includes('<span>People</span>'));
assert(indexSource.includes('id="channel-message-input" class="nr-input message-composer-input" rows="1" placeholder="Message..."'));
assert(indexSource.includes('class="channel-send-btn message-send-btn" id="channel-send-btn"'));
assert(!indexSource.includes('nr-btn nr-btn-primary channel-send-btn'));
assert(channelsSource.includes("'Message...'"));
assert(!channelsSource.includes('Message channel'));
assert(componentsCss.includes('.message-composer-input'));
assert(componentsCss.includes('border-radius: var(--radius-pill);'));
assert(componentsCss.includes('.message-send-btn::before'));
assert(componentsCss.includes('border-radius: var(--radius-full);'));
assert(indexSource.includes('/static/style.css?v=ui-20260826-1'));
assert(indexSource.includes('/static/js/nav.js?v=ui-20260826-1'));
assert(indexSource.includes('/static/js/ui_shared.js?v=ui-20260826-1'));
assert(indexSource.includes('/static/js/lxmf.js?v=ui-20260826-1'));
assert(indexSource.includes('/static/js/channels.js?v=ui-20260826-1'));
assert(indexSource.includes('/static/js/channel_hub.js?v=ui-20260826-1'));

process.stdout.write('Channels avatar tests passed.\n');
