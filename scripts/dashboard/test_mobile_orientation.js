#!/usr/bin/env node
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');

var repoRoot = path.join(__dirname, '..', '..');
function read(relative) {
    return fs.readFileSync(path.join(repoRoot, relative), 'utf8');
}

var androidManifest = read('src-tauri/gen/android/app/src/main/AndroidManifest.xml');
assert(androidManifest.includes('android:screenOrientation="portrait"'),
    'Android MainActivity must be locked to upright portrait');
assert(androidManifest.includes(
    'android:name="android.window.PROPERTY_COMPAT_ALLOW_RESTRICTED_RESIZABILITY"'),
    'Android 16 large screens must retain the temporary orientation compatibility mode');
assert(/PROPERTY_COMPAT_ALLOW_RESTRICTED_RESIZABILITY"\s+android:value="true"/.test(androidManifest),
    'Android orientation compatibility mode must be enabled');

var appleProject = read('src-tauri/gen/apple/project.yml');
var applePlist = read('src-tauri/gen/apple/ratspeak_iOS/Info.plist');

function yamlOrientationBlock(key) {
    var match = appleProject.match(new RegExp(
        '^\\s*' + key.replace('~', '\\~') + ':\\n((?:\\s+- UIInterfaceOrientation[^\\n]+\\n?)+)',
        'm'));
    assert(match, 'Apple project must declare ' + key);
    return match[1];
}

function plistOrientationBlock(key) {
    var match = applePlist.match(new RegExp(
        '<key>' + key.replace('~', '\\~') + '</key>\\s*<array>([\\s\\S]*?)</array>'));
    assert(match, 'Apple Info.plist must declare ' + key);
    return match[1];
}

[yamlOrientationBlock('UISupportedInterfaceOrientations'),
    plistOrientationBlock('UISupportedInterfaceOrientations')].forEach(function(source) {
    assert(source.includes('UIInterfaceOrientationPortrait'),
        'iPhone configuration must support portrait');
    assert(!source.includes('UIInterfaceOrientationLandscape'),
        'iPhone configuration must remain portrait-only');
});

[yamlOrientationBlock('UISupportedInterfaceOrientations~ipad'),
    plistOrientationBlock('UISupportedInterfaceOrientations~ipad')].forEach(function(source) {
    [
        'UIInterfaceOrientationPortrait',
        'UIInterfaceOrientationPortraitUpsideDown',
        'UIInterfaceOrientationLandscapeLeft',
        'UIInterfaceOrientationLandscapeRight'
    ].forEach(function(orientation) {
        assert(source.includes(orientation),
            'iPad configuration must support ' + orientation);
    });
});

console.log('Mobile orientation tests passed.');
