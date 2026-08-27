#!/usr/bin/env node
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var source = fs.readFileSync(
    path.join(__dirname, '..', 'static', 'js', 'settings.js'),
    'utf8'
);
var setupSource = fs.readFileSync(
    path.join(__dirname, '..', 'static', 'js', 'setup.js'),
    'utf8'
);
var start = source.indexOf('function _settingsNotificationActionForState');
var end = source.indexOf('\n(function() {', start);
assert(start >= 0 && end > start, 'notification state reducer must exist');
var context = {};
vm.runInNewContext(source.slice(start, end), context, { filename: 'mobile-notifications.js' });

assert.deepStrictEqual(
    JSON.parse(JSON.stringify(context._settingsNotificationActionForState('prompt'))),
    { hidden: false, disabled: false, label: 'Allow' },
    'fresh installs must expose a visible permission action'
);
assert.deepStrictEqual(
    JSON.parse(JSON.stringify(context._settingsNotificationActionForState('denied'))),
    { hidden: false, disabled: false, label: 'Open Settings' },
    'denied permission must expose system recovery'
);
assert.deepStrictEqual(
    JSON.parse(JSON.stringify(context._settingsNotificationActionForState('granted'))),
    { hidden: false, disabled: false, label: 'Review' },
    'granted permission must keep device sound and haptics settings reachable'
);
assert.deepStrictEqual(
    JSON.parse(JSON.stringify(context._settingsNotificationActionForState('unavailable'))),
    { hidden: false, disabled: true, label: 'Unavailable' },
    'unavailable native state must fail closed and remain honest'
);
assert.deepStrictEqual(
    JSON.parse(JSON.stringify(context._settingsNotificationPresentation(false, 'prompt'))),
    { hidden: true, disabled: true, label: 'Allow' },
    'a persisted disabled app preference must hide the operating-system action'
);
assert.deepStrictEqual(
    JSON.parse(JSON.stringify(context._settingsNotificationPresentation(true, 'prompt'))),
    { hidden: false, disabled: false, label: 'Allow' },
    'an enabled app preference must surface a fresh permission request'
);
assert(source.includes("document.addEventListener('rs-notification-permission-changed'"),
    'the row must refresh after the Android permission result');
assert(source.includes("window.addEventListener('focus'"),
    'the row must refresh after returning from system settings');
assert(setupSource.includes('requestSetupNotificationPermissionIfEnabled();'),
    'existing and newly completed mobile identities must enter the one-time permission path');
var existingIdentityBranch = setupSource.indexOf("document.body.classList.remove('setup-active')");
assert(existingIdentityBranch >= 0 &&
    setupSource.indexOf('requestSetupNotificationPermissionIfEnabled();', existingIdentityBranch) >= 0,
    'an existing identity upgrading to native notifications must not silently stay unprompted');

console.log('Mobile notification settings tests passed');
