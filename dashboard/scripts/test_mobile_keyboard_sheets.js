#!/usr/bin/env node
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var root = path.join(__dirname, '..');
var navSource = fs.readFileSync(path.join(root, 'static', 'js', 'nav.js'), 'utf8');
var dialogsSource = fs.readFileSync(path.join(root, 'static', 'js', 'dialogs.js'), 'utf8');
var responsiveCss = fs.readFileSync(path.join(root, 'static', 'css', '13-responsive.css'), 'utf8');
var androidActivity = fs.readFileSync(path.join(
    root, '..', 'src-tauri', 'gen', 'android', 'app', 'src', 'main', 'java',
    'org', 'ratspeak', 'android', 'MainActivity.kt'
), 'utf8');

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

var context = {
    document: { activeElement: null, documentElement: {} },
    window: {
        innerHeight: 852,
        visualViewport: { height: 500, offsetTop: 0 }
    },
    getComputedStyle: function() {
        return { getPropertyValue: function() { return '12px'; } };
    }
};
vm.createContext(context);
vm.runInContext(
    namedFunctionSource(navSource, '_visualViewportGeometry') +
        '\nthis.geometry = _visualViewportGeometry;' +
    namedFunctionSource(navSource, '_keyboardViewportMetrics') +
        '\nthis.metrics = _keyboardViewportMetrics;' +
    namedFunctionSource(navSource, '_revealFocusedSheetField') +
        '\nthis.reveal = _revealFocusedSheetField;',
    context,
    { filename: 'mobile-keyboard-sheet-policy.js' }
);

assert.deepStrictEqual(
    JSON.parse(JSON.stringify(context.geometry(500, 0, 0, 0, 0))),
    { height: 500, offset: 0 },
    'an unpanned conversation must begin at the visual viewport origin'
);
assert.deepStrictEqual(
    JSON.parse(JSON.stringify(context.geometry(500, 300, 300, 0, 0))),
    { height: 500, offset: 300 },
    'an iOS-panned conversation must follow the visual viewport top'
);
assert.deepStrictEqual(
    JSON.parse(JSON.stringify(context.geometry(500, 0, 300, 0, 0))),
    { height: 500, offset: 300 },
    'pageTop must cover the WebKit frame where offsetTop updates late'
);
assert.deepStrictEqual(
    JSON.parse(JSON.stringify(context.geometry(500, 0, 0, -84, 0))),
    { height: 500, offset: 84 },
    'a WKWebView-rendered body shift must be compensated even when scrollTop stays zero'
);
assert.deepStrictEqual(
    JSON.parse(JSON.stringify(context.geometry(500, 0, 100, -100, 100))),
    { height: 500, offset: 0 },
    'ordinary document scrolling must not be mistaken for a keyboard pan'
);

assert.deepStrictEqual(
    JSON.parse(JSON.stringify(context.metrics(852, 852, 0, 852))),
    { open: false, inset: 0 },
    'the unobscured iPhone viewport must remain at the sheet baseline'
);
assert.deepStrictEqual(
    JSON.parse(JSON.stringify(context.metrics(852, 500, 0, 852))),
    { open: true, inset: 352 },
    'an unpanned iOS visual viewport must lift a fixed sheet to the keyboard edge'
);
assert.deepStrictEqual(
    JSON.parse(JSON.stringify(context.metrics(852, 500, 300, 852))),
    { open: true, inset: 52 },
    'an iOS viewport panned toward the field must use its visible bottom edge'
);
assert.deepStrictEqual(
    JSON.parse(JSON.stringify(context.metrics(500, 500, 0, 852))),
    { open: true, inset: 0 },
    'a platform that resizes both viewports must not add a second keyboard offset'
);

var scrollOwner = {
    scrollTop: 20,
    getBoundingClientRect: function() { return { top: 100, bottom: 400 }; }
};
var sheet = {};
var field = {
    closest: function(selector) {
        if (selector === '.bottom-sheet.open') return sheet;
        if (selector === '.bottom-sheet-body') return scrollOwner;
        return null;
    },
    getBoundingClientRect: function() { return { top: 380, bottom: 430 }; }
};
context.document.activeElement = field;
context.reveal(field);
assert.strictEqual(scrollOwner.scrollTop, 62,
    'a keyboard-covered sheet field must scroll inside the canonical sheet body');

field.getBoundingClientRect = function() { return { top: 70, bottom: 120 }; };
context.reveal(field);
assert.strictEqual(scrollOwner.scrollTop, 20,
    'a field above the visible body must scroll back into view');

var inactive = Object.assign({}, field);
context.document.activeElement = inactive;
context.reveal(field);
assert.strictEqual(scrollOwner.scrollTop, 20,
    'a stale focus callback must not move a newly focused form');

var contactPrompt = namedFunctionSource(dialogsSource, 'rsPromptContact');
assert(contactPrompt.includes("_rsBuildSheet({ title: opts.title || 'Add Contact' }"),
    'Add Contact must inherit the canonical keyboard-aware sheet');
assert(navSource.includes("style.setProperty('--visual-viewport-height'"));
assert(navSource.includes("style.setProperty('--visual-viewport-top'"));
assert(navSource.includes("style.setProperty('--keyboard-inset'"));
assert(navSource.includes("'data-keyboard-platform'"));
assert(!navSource.includes('window.ratspeakApplyNativeImeGeometry'));
assert(navSource.includes("if (isIOS()) window.addEventListener('resize', onResize)"),
    'the extra window resize clock must remain iOS-only');
assert(navSource.includes("behavior: 'auto'"),
    'iOS keyboard-owned reveals must not add a second smooth-scroll delay');
assert(navSource.includes("el.scrollIntoView({ block: 'nearest', behavior: 'smooth' });"),
    'Android must retain the last known-good native-resize reveal behavior');
assert(!navSource.includes('_scheduleFocusedFieldReveal(el, 180)'),
    'form fields must not wait for a post-keyboard timeout');
assert(navSource.includes("el.id === 'lxmf-input' || el.id === 'channel-message-input'"),
    'Direct Message and Channel composers must share the viewport owner');
assert(!navSource.includes('scheduleViewportSettle'),
    'composer focus must not wait for delayed viewport resampling');
assert(navSource.includes("active.closest('.bottom-sheet.open')"),
    'iOS visual viewport changes must re-reveal the focused sheet field');
assert(navSource.includes("// Preserve the Android pipeline used through v1.0.29:"));
assert(navSource.includes("style.setProperty('--app-height', currentHeight + 'px')"),
    'Android conversations must receive live visualViewport height again');
assert(navSource.includes("if (isIOS()) onResize();"),
    'Android keeps the historical visualViewport event owner');
assert(responsiveCss.includes('html[data-keyboard-platform="ios"].keyboard-open .bottom-sheet.open:focus-within'));
assert(responsiveCss.includes('bottom: var(--keyboard-inset, 0px);'));
assert(responsiveCss.includes('var(--visual-viewport-height, 100dvh)'));
assert(responsiveCss.includes('html[data-keyboard-platform="ios"].keyboard-open body.view-chat-detail .app-layout'));
assert(responsiveCss.includes('html[data-keyboard-platform="ios"].keyboard-open body.view-channel-detail .app-layout'));
assert(responsiveCss.includes('html[data-keyboard-platform="ios"] body.view-chat-detail .app-layout,\n    html[data-keyboard-platform="ios"] body.view-channel-detail .app-layout'),
    'a lingering iOS viewport pan must remain compensated after keyboard dismissal');
assert(responsiveCss.includes('top: var(--visual-viewport-top, 0px);'));
assert(androidActivity.includes('view.setPadding(bars.left, 0, bars.right, ime.bottom)'));
assert(!androidActivity.includes('appOwnsVisibleIme'));
assert(!androidActivity.includes('inputMethod.isActive(webView)'));
assert(!androidActivity.includes('WindowInsetsCompat.Builder(insets)'));
assert(!androidActivity.includes('.setInsets(WindowInsetsCompat.Type.ime(), Insets.NONE)'));
assert(!androidActivity.includes('WindowInsetsAnimationCompat.Callback'));

console.log('Mobile keyboard-safe sheet behavior tests passed');
