#!/usr/bin/env node
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var dashboardRoot = path.join(__dirname, '..');
var navSource = fs.readFileSync(path.join(dashboardRoot, 'static', 'js', 'nav.js'), 'utf8');
var channelsSource = fs.readFileSync(path.join(dashboardRoot, 'static', 'js', 'channels.js'), 'utf8');
var dialogsSource = fs.readFileSync(path.join(dashboardRoot, 'static', 'js', 'dialogs.js'), 'utf8');
var constantsSource = fs.readFileSync(path.join(dashboardRoot, 'static', 'js', 'constants.js'), 'utf8');
var contactCardSource = fs.readFileSync(path.join(dashboardRoot, 'static', 'js', 'contact_card.js'), 'utf8');
var indexSource = fs.readFileSync(path.join(dashboardRoot, 'index.html'), 'utf8');
var channelsCss = fs.readFileSync(path.join(dashboardRoot, 'static', 'css', '09-channels.css'), 'utf8');
var responsiveCss = fs.readFileSync(path.join(dashboardRoot, 'static', 'css', '13-responsive.css'), 'utf8');

assert(navSource.includes("var MOBILE_TAB_SLOTS = ['peers', 'message', 'channels', 'more'];"),
    'mobile swipes must traverse the four visible bottom-bar destinations');
assert(navSource.includes("var MORE_VIEWS = ['contacts', 'identity', 'network', 'games', 'settings'];"),
    'Contacts must route through More after Channels takes its bottom-bar slot');
assert(!navSource.includes("if (viewId === 'channels') return 'message';"),
    'Channels must own its selected state instead of aliasing Direct Messages');
assert(!indexSource.includes('channel-hub-add-btn'),
    'mobile should use the hub card itself instead of a redundant add control');
assert(indexSource.includes('id="channel-hub-summary" type="button"'));
assert(indexSource.includes('data-channel-action="hub-actions"'));
assert(indexSource.includes('id="channel-hub-menu-btn" type="button" title="Manage Hub"'));
assert(responsiveCss.includes('.channels-sidebar-header {\n        display: none;'),
    'mobile Channels should begin with the hub card instead of a redundant title');

function functionSource(source, name) {
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

function classList(initial) {
    var values = new Set(initial || []);
    return {
        contains: function(value) { return values.has(value); },
        add: function(value) { values.add(value); },
        remove: function(value) { values.delete(value); }
    };
}

var firstDismissals = 0;
var topDismissals = 0;
var first = {
    classList: classList(['bottom-sheet', 'open']),
    _ratspeakDismiss: function() { firstDismissals += 1; }
};
var top = {
    classList: classList(['bottom-sheet', 'open']),
    _ratspeakDismiss: function() { topDismissals += 1; }
};
var persistent = { classList: classList(['bottom-sheet', 'open']) };
var persistentOverlay = { classList: classList(['active']) };
var openSheets = [first, top];

var context = {
    document: {
        querySelectorAll: function(selector) {
            assert.strictEqual(selector, '.rs-sheet-shell.open, .bottom-sheet.open');
            return openSheets;
        },
        getElementById: function(id) {
            if (id === 'bottom-sheet') return persistent;
            if (id === 'bottom-sheet-overlay') return persistentOverlay;
            return null;
        }
    }
};
vm.createContext(context);
vm.runInContext(functionSource(navSource, '_closeOpenBottomSheet'), context);

assert.strictEqual(context._closeOpenBottomSheet(), true);
assert.strictEqual(topDismissals, 1, 'native Back dismisses the topmost dynamic sheet');
assert.strictEqual(firstDismissals, 0, 'native Back leaves lower sheets alone');
assert.strictEqual(persistent.classList.contains('open'), true,
    'native Back must not close a sheet underneath the topmost dialog');

openSheets = [persistent];
assert.strictEqual(context._closeOpenBottomSheet(), true);
assert.strictEqual(persistent.classList.contains('open'), false,
    'the persistent More sheet keeps its existing native Back behavior');
assert.strictEqual(persistentOverlay.classList.contains('active'), false);

var drillSwipe = functionSource(navSource, 'initDrillDownSwipeBack');
var tabSwipe = functionSource(navSource, 'initTabSwipe');
assert(drillSwipe.includes('if (_isMobileNavigationBlocked()) return true;'),
    'edge-swipe Back must not navigate beneath an open sheet');
assert(tabSwipe.includes('if (_isMobileNavigationBlocked()) return true;'),
    'tab swipes must not navigate beneath an open sheet');
assert(navSource.includes("'.bottom-sheet.open, .bottom-sheet-overlay.active, '"));
assert(navSource.includes('.channels-layout.members-open'));
assert(constantsSource.includes('sheet._ratspeakDismiss = function()'),
    'all dynamic sheet shells need a native Back dismissal hook');
assert(constantsSource.includes("sheet.className = 'rs-sheet-shell '"),
    'custom runtime sheets need a style-free marker for native Back');
assert(contactCardSource.includes('built.sheet._ratspeakDismiss = closeAll;'),
    'the QR scanner must stop its camera through its native Back hook');
assert(dialogsSource.includes('return _rsHandleNativeBack(opts, dismiss);'),
    'rich sheets must preserve their state-aware dismissal callback');

var dialogBackContext = {};
vm.createContext(dialogBackContext);
vm.runInContext(functionSource(dialogsSource, '_rsHandleNativeBack'), dialogBackContext);
var dismissedValues = [];
function recordDismiss(value) { dismissedValues.push(value); }
assert.strictEqual(dialogBackContext._rsHandleNativeBack({}, recordDismiss), true);
assert.deepStrictEqual(dismissedValues, [null],
    'ordinary rich sheets resolve their documented null cancel value');
dismissedValues = [];
assert.strictEqual(dialogBackContext._rsHandleNativeBack({
    nativeBackValue: function() { return false; }
}, recordDismiss), true);
assert.deepStrictEqual(dismissedValues, [false],
    'confirm dialogs preserve false-on-cancel through native Back');
dismissedValues = [];
assert.strictEqual(dialogBackContext._rsHandleNativeBack({
    nativeBackValue: function() { return { confirmed: false, checked: true }; }
}, recordDismiss), true);
assert.deepStrictEqual(JSON.parse(JSON.stringify(dismissedValues)), [
    { confirmed: false, checked: true }
], 'stateful dialog cancel values survive native Back');
dismissedValues = [];
assert.strictEqual(dialogBackContext._rsHandleNativeBack({
    nativeBackDismissible: false
}, recordDismiss), false);
assert.deepStrictEqual(dismissedValues, [],
    'native Back is consumed without closing a non-dismissible progress sheet');

var showDialogSource = functionSource(dialogsSource, '_rsShowDialog');
assert(showDialogSource.includes('nativeBackValue: function() { return resolveValue(false); }'),
    'prompt, confirm, and checkbox sheets derive the same cancel value for native Back');
var progressSource = functionSource(dialogsSource, 'rsProgress');
assert(progressSource.includes('nativeBackDismissible: false'),
    'progress sheets keep their non-dismissible contract on native Back');

var memberPaneContext = {
    _channelsSelectedMemberKey: 'member-id',
    _channelsShowMemberList: function() { memberPaneContext.listShows += 1; },
    channelsCloseMemberPane: function() { memberPaneContext.closes += 1; },
    listShows: 0,
    closes: 0,
    layout: { classList: classList(['members-open']) },
    _channelsEl: function(id) {
        return id === 'channels-layout' ? memberPaneContext.layout : null;
    }
};
vm.createContext(memberPaneContext);
vm.runInContext(functionSource(channelsSource, 'channelsHandleMemberPaneBack'), memberPaneContext);
assert.strictEqual(memberPaneContext.channelsHandleMemberPaneBack(), true);
assert.strictEqual(memberPaneContext.listShows, 1,
    'Back from member detail returns to the member list');
assert.strictEqual(memberPaneContext.closes, 0);
memberPaneContext._channelsSelectedMemberKey = null;
assert.strictEqual(memberPaneContext.channelsHandleMemberPaneBack(), true);
assert.strictEqual(memberPaneContext.closes, 1,
    'Back from the member list closes the member sheet');
memberPaneContext.layout.classList.remove('members-open');
assert.strictEqual(memberPaneContext.channelsHandleMemberPaneBack(), false);

var backHandler = functionSource(navSource, '_handleAppBackNavigation');
assert(backHandler.indexOf('channelsHandleMemberPaneBack()') <
        backHandler.indexOf('RS.viewStack.depth()'),
    'member-sheet Back handling must run before room navigation');

process.stdout.write('Mobile sheet navigation tests passed.\n');
