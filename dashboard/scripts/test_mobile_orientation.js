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
[appleProject, applePlist].forEach(function(source) {
    assert(source.includes('UIInterfaceOrientationPortrait'),
        'Apple mobile configuration must support portrait');
    assert(!source.includes('UIInterfaceOrientationLandscape'),
        'Apple mobile configuration must not advertise landscape');
});

console.log('Mobile portrait-orientation tests passed.');
