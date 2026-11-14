#!/usr/bin/env node
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var source = fs.readFileSync(
    path.join(__dirname, '..', '..', 'dashboard', 'static', 'js', 'channels.js'),
    'utf8'
);

function functionSource(name) {
    var start = source.indexOf('\nfunction ' + name + '(');
    assert.notStrictEqual(start, -1, name + ' must exist');
    var end = source.indexOf('\n}\n', start);
    return source.slice(start + 1, end + 2);
}

function element(tagName) {
    return {
        tagName: tagName,
        className: '',
        textContent: '',
        dateTime: '',
        title: '',
        dataset: {},
        children: [],
        appendChild: function(child) { this.children.push(child); },
        setAttribute: function() {},
        addEventListener: function() {}
    };
}

var kitSource = fs.readFileSync(
    path.join(__dirname, '..', '..', 'dashboard', 'static', 'js', 'ui_kit.js'),
    'utf8'
);
var context = {
    Date: Date,
    Number: Number,
    channelsSnapshot: { nickname: 'Bob' },
    document: {
        createElement: element,
        createTextNode: function(text) { return { textContent: text }; },
        addEventListener: function() {}
    },
    _channelsCanCompose: function() { return false; },
    _channelsIsHubNotice: function() { return false; },
    _channelsBuildHubNotice: function() { throw new Error('not a hub notice'); },
    _channelsBuildMessageActions: function() { return null; },
    _channelsBindTouchReplyAction: function() {},
    _channelsActivityTime: function(item) { return Number(item.timestamp_ms) || 0; },
    _channelsIdentityAvatarSeed: function(sourceHash, lxmfHash) { return lxmfHash || sourceHash; },
    _channelsPopulateIdentityAvatar: function(avatar, seed, size) {
        avatar.avatarSeed = seed;
        avatar.avatarSize = size;
    },
    _channelsShortHash: function() { return 'peer'; }
};
context.window = context;
vm.createContext(context);
vm.runInContext(kitSource, context, { filename: 'ui_kit.js' });
vm.runInContext(
    functionSource('_channelsDisplayDate') + '\n' +
    functionSource('_channelsDisplayTime') + '\n' +
    functionSource('_channelsAppendMessageText') + '\n' +
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
}, false, 'single');
var avatar = rendered.children[0];
var head = rendered.children[1];
var time = head.children.find(function(child) { return child.tagName === 'time'; });
assert(time, 'the run head keeps a semantic time element beside the author');
assert.doesNotThrow(function() { new Date(time.dateTime).toISOString(); });
assert.notStrictEqual(time.dateTime, 'Invalid Date');
assert(!/NaN/.test(time.textContent) && !/NaN/.test(time.title),
    'an out-of-range peer timestamp still shows a real clock time');
assert(avatar.className.indexOf('channel-event-avatar') !== -1 &&
    avatar.className.indexOf('ui-avatar--32') !== -1);
assert.strictEqual(avatar.avatarSeed, '22'.repeat(16));
assert.strictEqual(avatar.avatarSize, 32);
assert.strictEqual(rendered.children[2].className, 'ch-msg-text');
assert.strictEqual(rendered.children[2].children.map(function(node) { return node.textContent; }).join(''),
    'still renders');

// A continued message in a run drops the avatar row and head, and keeps the time as a tooltip.
var continued = context._channelsBuildTranscriptItem({
    kind: 'message', timestamp_ms: 1700000000000, nickname: 'Remote rat', text: 'second line', ours: false
}, false, 'end');
assert(continued.className.indexOf('ch-msg--cont') !== -1);
assert(continued.children.every(function(child) { return child.className !== 'ch-msg-head'; }));
assert(continued.title, 'a continued message carries its full time as a tooltip');

// Mentions: the row is tinted and your own @name is the highlighted one.
var mention = context._channelsBuildTranscriptItem({
    kind: 'message', timestamp_ms: 1700000000000, nickname: 'mara',
    text: '@bob, and @theo: ridge at six?', ours: false, mentioned: true
}, false, 'single');
assert(mention.className.indexOf('is-mention') !== -1);
var spans = mention.children[2].children.filter(function(node) { return node.className; });
assert.deepStrictEqual(spans.map(function(node) { return node.className + ':' + node.textContent; }),
    ['ch-at is-you:@bob', 'ch-at:@theo']);
assert.strictEqual(mention.children[2].children.map(function(node) { return node.textContent; }).join(''),
    '@bob, and @theo: ridge at six?', 'highlighting never changes the text');

process.stdout.write('Channels timestamp rendering tests passed.\n');
