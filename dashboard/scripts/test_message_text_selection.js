#!/usr/bin/env node
// Message text stays natively selectable without stealing the reaction gesture.

'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var dashboardRoot = path.join(__dirname, '..');
var lxmfSource = fs.readFileSync(path.join(dashboardRoot, 'static', 'js', 'lxmf.js'), 'utf8');
var messagingCss = fs.readFileSync(path.join(dashboardRoot, 'static', 'css', '09-messaging.css'), 'utf8');
var responsiveCss = fs.readFileSync(path.join(dashboardRoot, 'static', 'css', '13-responsive.css'), 'utf8');

function namedFunctionSource(source, name) {
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

assert(/\.lxmf-msg\s*\{[\s\S]*?user-select:\s*none;[\s\S]*?-webkit-user-select:\s*none;/.test(messagingCss),
    'message chrome must remain non-selectable');
assert(/\.lxmf-msg-content\s*\{[\s\S]*?user-select:\s*text;[\s\S]*?-webkit-user-select:\s*text;[\s\S]*?-webkit-touch-callout:\s*default;/.test(messagingCss),
    'message bodies must opt into native selection and copy callouts');
assert(/@media \(max-width: 768px\)[\s\S]*?\.lxmf-msg-content\s*\{[\s\S]*?-webkit-user-select:\s*text;[\s\S]*?user-select:\s*text;[\s\S]*?-webkit-touch-callout:\s*default;/.test(responsiveCss),
    'mobile message bodies must override the non-selectable chrome policy');

var selectedNode = {};
var selection = {
    isCollapsed: false,
    rangeCount: 1,
    getRangeAt: function() {
        return {
            commonAncestorContainer: selectedNode,
            intersectsNode: function(node) { return node === bubble; }
        };
    }
};
var context = {
    window: { getSelection: function() { return selection; } }
};
vm.runInNewContext(
    namedFunctionSource(lxmfSource, '_messageSelectionIntersectsBubble') + '\n' +
    namedFunctionSource(lxmfSource, '_messageTouchTargetsSelectableText') + '\n' +
    'this.selectionIntersects = _messageSelectionIntersectsBubble;\n' +
    'this.touchTargetsText = _messageTouchTargetsSelectableText;',
    context,
    { filename: 'message-selection-policy.js' }
);

var bubble = {
    contains: function(node) { return node === selectedNode || node === textTarget; }
};
var textTarget = {
    closest: function(selector) { return selector === '.lxmf-msg-content' ? textTarget : null; }
};
var chromeTarget = {
    closest: function() { return null; }
};

assert.strictEqual(context.selectionIntersects(bubble), true);
selection.isCollapsed = true;
assert.strictEqual(context.selectionIntersects(bubble), false);
assert.strictEqual(context.touchTargetsText({ target: textTarget }, bubble), true);
assert.strictEqual(context.touchTargetsText({ target: chromeTarget }, bubble), false);

assert(lxmfSource.includes('_messageTouchTargetsSelectableText(touch, bubble) ||'),
    'text-origin long presses must not open message actions');
assert(lxmfSource.includes("document.documentElement.dataset.inputModality === 'touch'"),
    'touch context menus on message text must remain native');
assert(lxmfSource.includes('touchModality || _messageSelectionIntersectsBubble(this)'),
    'an existing selection must retain the native copy menu');

console.log('Message text selection tests passed.');
