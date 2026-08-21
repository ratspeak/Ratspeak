#!/usr/bin/env node
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var dashboardRoot = path.join(__dirname, '..');
var source = fs.readFileSync(path.join(dashboardRoot, 'static', 'js', 'lxmf.js'), 'utf8');

function functionSource(name) {
    var start = source.indexOf('function ' + name + '(');
    assert.notStrictEqual(start, -1, name + ' must exist');
    var brace = source.indexOf('{', start);
    var depth = 0;
    for (var index = brace; index < source.length; index++) {
        if (source[index] === '{') depth++;
        if (source[index] === '}') {
            depth--;
            if (depth === 0) return source.slice(start, index + 1);
        }
    }
    throw new Error('unterminated function ' + name);
}

var context = {
    _messageProgressPercent: function() { return null; }
};
vm.createContext(context);
vm.runInContext(functionSource('_messageStateIconHtml'), context);

function icon(state, method) {
    return context._messageStateIconHtml({
        state: state,
        delivery_method: method,
        direction: 'outbound'
    });
}

function assertSent(method) {
    var html = icon('sent', method);
    assert(html.includes('msg-state-sent'), method + ' sent must use the muted sent treatment');
    assert(html.includes('aria-label="Sent"'), method + ' sent must remain accessible as Sent');
    assert(!html.includes('msg-state-delivered'), method + ' sent must not claim delivery');
}

function assertDelivered(method) {
    var html = icon('delivered', method);
    assert(html.includes('msg-state-delivered'), method + ' proof must use the confirmed treatment');
    assert(html.includes('aria-label="Delivered"'), method + ' proof must be accessible as Delivered');
    assert(!html.includes('msg-state-sent'), method + ' proof must not be collapsed back to Sent');
}

['opportunistic', 'direct'].forEach(assertSent);
['opportunistic', 'direct'].forEach(assertDelivered);
assertDelivered(undefined);

var propagated = icon('propagated', 'propagated');
assert(propagated.includes('msg-state-propagated'));
assert(propagated.includes('aria-label="Stored in Offline Inbox"'));

var failed = icon('failed', 'opportunistic');
assert(failed.includes('msg-state-failed'));
assert(failed.includes('aria-label="Failed"'));

var stopped = icon('cancelled', 'direct');
assert(stopped.includes('aria-label="Sending stopped"'));
assert(source.includes('aria-label="Stop sending message">Stop</button>'));
assert(source.includes("message: 'Stop preparing and retrying this message?'"));
assert(source.includes('Stopped retrying. A copy already handed to the network may still arrive.'));
assert(source.includes('No live send remained. The local message was marked stopped, but a copy may still arrive.'));
assert(source.includes('Stopped before the message left this device.'));
assert(source.includes("title: 'Stop sending?'"));

var css = fs.readFileSync(path.join(dashboardRoot, 'static', 'css', '09-messaging.css'), 'utf8');
assert(/\.msg-state-sent svg\s*\{[^}]*var\(--text-muted\)/s.test(css),
    'sent checks must use the muted foreground');
assert(/\.msg-state-delivered svg\s*\{[^}]*var\(--accent\)/s.test(css),
    'proof-confirmed checks must use the accent foreground');

console.log('LXMF delivery icon tests passed');
