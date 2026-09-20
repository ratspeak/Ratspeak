#!/usr/bin/env node
'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const source = fs.readFileSync(path.join(__dirname, '../../dashboard/static/js/lxmf.js'), 'utf8');
const context = {};
vm.createContext(context);
for (const name of ['_messageDisplayContent', '_conversationPreviewForMessage']) {
    const start = source.indexOf('function ' + name + '(');
    assert(start >= 0);
    vm.runInContext(source.slice(start, source.indexOf('\nfunction ', start + 1)), context);
}
const display = context._messageDisplayContent;
assert.equal(display({title: ' A title ', content: ''}, false), 'A title');
assert.equal(display({title: '日本語', content: 'Привет'}, false), '日本語\n\nПривет');
assert.equal(display({title: ' ', content: 'Original\nbody'}, false), 'Original\nbody');
assert.equal(display({title: '<script>', content: 'body'}, false), '<script>\n\nbody');
assert.equal(display({title: 'Photo title', content: '[File: photo.png]', image: {}}, false), 'Photo title');
assert.equal(display({title: 'Memo title', content: 'Voice message'}, true), 'Memo title');
assert.equal(display({content: 'Voice message'}, true), '');
assert.equal(context._conversationPreviewForMessage({title: 'Title', content: ''}), 'Title');
assert.equal(context._conversationPreviewForMessage({audio: {mode: 0xfe}}), 'Unsupported audio');
assert(source.includes('linkifyMessageText(displayContent)'), 'title text uses the existing escaped renderer');
assert(source.includes('var replyContent = _messageDisplayContent(msgData, !!msgData.audio)'), 'reply quotes include titles');
assert(source.includes('var messageText = _messageDisplayContent(msgData, !!(msgData && msgData.audio))'), 'Copy Message includes titles');
assert(source.includes('var notifBody = _messageDisplayContent(msg, !!msg.audio)'), 'browser notifications include titles');
console.log('message title/body presentation: PASS');
