#!/usr/bin/env node
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var source = fs.readFileSync(
    path.join(__dirname, '..', 'static', 'js', 'channels.js'),
    'utf8'
);

function functionSource(name) {
    var start = source.indexOf('function ' + name + '(');
    assert.notStrictEqual(start, -1, name + ' must exist');
    var brace = source.indexOf('{', start);
    var depth = 0;
    for (var index = brace; index < source.length; index++) {
        if (source[index] === '{') depth += 1;
        if (source[index] === '}') {
            depth -= 1;
            if (depth === 0) return source.slice(start, index + 1);
        }
    }
    throw new Error('unterminated function ' + name);
}

function element(tagName) {
    return {
        tagName: tagName,
        className: '',
        textContent: '',
        dateTime: '',
        dataset: {},
        children: [],
        appendChild: function(child) { this.children.push(child); },
        setAttribute: function() {},
        addEventListener: function() {}
    };
}

var context = {
    Date: Date,
    Number: Number,
    channelsSnapshot: { nickname: 'Bob' },
    document: { createElement: element },
    _channelsCanCompose: function() { return false; },
    _channelsIsHubNotice: function() { return false; },
    _channelsBuildHubNotice: function() { throw new Error('not a hub notice'); },
    _channelsBuildQuoteButton: function() { return null; },
    _channelsBindTouchReplyAction: function() {},
    _channelsIdentityAvatarSeed: function(sourceHash, lxmfHash) { return lxmfHash || sourceHash; },
    _channelsPopulateIdentityAvatar: function(avatar, seed, size) {
        avatar.avatarSeed = seed;
        avatar.avatarSize = size;
    },
    _channelsIdentityTone: function() { return '0'; },
    _channelsShortHash: function() { return 'peer'; }
};
vm.createContext(context);
vm.runInContext(
    functionSource('_channelsDisplayDate') + '\n' +
    functionSource('_channelsFormatTime') + '\n' +
    functionSource('_channelsBuildTranscriptItem'),
    context
);

var rendered = context._channelsBuildTranscriptItem({
    kind: 'message',
    timestamp_ms: '18446744073709551615',
    source_hash: '11'.repeat(16),
    source_lxmf_hash: '22'.repeat(16),
    nickname: 'Remote rat',
    text: 'still renders',
    ours: false
}, false);
var avatar = rendered.children[0];
var meta = rendered.children[2];
var time = meta.children.find(function(child) { return child.tagName === 'time'; });
assert(time, 'message metadata must retain a semantic time element beside its actions');
assert.doesNotThrow(function() { new Date(time.dateTime).toISOString(); });
assert.notStrictEqual(time.dateTime, 'Invalid Date');
assert.strictEqual(avatar.className, 'channel-event-avatar');
assert.strictEqual(avatar.avatarSeed, '22'.repeat(16));
assert.strictEqual(avatar.avatarSize, 32);
assert.strictEqual(rendered.children[3].textContent, 'still renders');

process.stdout.write('Channels timestamp rendering tests passed.\n');
