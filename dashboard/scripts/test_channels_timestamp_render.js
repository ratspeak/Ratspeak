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
        setAttribute: function() {}
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
    _channelsBuildPresenceEvent: function() { throw new Error('not presence'); },
    _channelsBuildQuoteButton: function() { return null; },
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
    nickname: 'Remote rat',
    text: 'still renders',
    ours: false
}, false);
var meta = rendered.children[1];
var time = meta.children[0];
assert.doesNotThrow(function() { new Date(time.dateTime).toISOString(); });
assert.notStrictEqual(time.dateTime, 'Invalid Date');
assert.strictEqual(rendered.children[2].textContent, 'still renders');

process.stdout.write('Channels timestamp rendering tests passed.\n');
