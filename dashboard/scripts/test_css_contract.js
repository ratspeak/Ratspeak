#!/usr/bin/env node
'use strict';

var assert = require('assert');
var childProcess = require('child_process');
var fs = require('fs');
var path = require('path');

var dashboardRoot = path.join(__dirname, '..');
var cssRoot = path.join(dashboardRoot, 'static', 'css');
var modules = [
    '00-tokens.css',
    '01-reset.css',
    '02-typography.css',
    '03-scrollbar.css',
    '04-layout.css',
    '05-panels.css',
    '06-forms.css',
    '07-components.css',
    '08-modals.css',
    '09-messaging.css',
    '09-channels.css',
    '10-views.css',
    '11-games.css',
    '12-animations.css',
    '13-responsive.css'
];

var sources = modules.map(function(name) {
    return {
        name: name,
        text: fs.readFileSync(path.join(cssRoot, name), 'utf8')
    };
});
var expectedBundle = sources.map(function(source) { return source.text + '\n'; }).join('');
var shellBuilder = fs.readFileSync(path.join(dashboardRoot, 'build-css.sh'), 'utf8');
var shellModules = Array.from(shellBuilder.matchAll(/^\s+([0-9][^\s]+\.css)\s*$/gm), function(match) { return match[1]; });
assert.deepStrictEqual(shellModules, modules, 'the shell CSS builder must use the canonical module order');
assert(!/\bsed\b|\bminif/i.test(shellBuilder), 'the shell CSS builder must not rewrite only its output');

var rustBuilder = fs.readFileSync(path.join(dashboardRoot, '..', 'src-tauri', 'build.rs'), 'utf8');
var rustModules = Array.from(rustBuilder.matchAll(/"([0-9][^"]+\.css)"/g), function(match) { return match[1]; });
assert.deepStrictEqual(rustModules, modules, 'the Cargo CSS builder must use the canonical module order');

if (process.platform !== 'win32') {
    childProcess.execFileSync(path.join(dashboardRoot, 'build-css.sh'), [], {
        cwd: path.dirname(dashboardRoot),
        stdio: 'pipe'
    });
}
var bundlePath = path.join(dashboardRoot, 'static', 'style.css');
var actualBundle = fs.existsSync(bundlePath) ? fs.readFileSync(bundlePath, 'utf8') : expectedBundle;
assert.strictEqual(actualBundle, expectedBundle,
    'static/style.css must be the exact deterministic module concatenation');

var allCss = sources.map(function(source) { return source.text; }).join('\n');
var defined = new Set();
var definitionRe = /(--[a-zA-Z0-9_-]+)\s*:/g;
var match;
while ((match = definitionRe.exec(allCss))) defined.add(match[1]);

var missing = [];
sources.forEach(function(source) {
    var referenceRe = /var\(\s*(--[a-zA-Z0-9_-]+)\s*([^)]*)\)/g;
    var ref;
    while ((ref = referenceRe.exec(source.text))) {
        if (/^\s*,/.test(ref[2])) continue;
        if (!defined.has(ref[1])) missing.push(source.name + ': ' + ref[1]);
    }
});
assert.deepStrictEqual(missing, [], 'every no-fallback CSS variable reference must be defined');

var keyframes = new Map();
sources.forEach(function(source) {
    var keyframeRe = /@keyframes\s+([a-zA-Z0-9_-]+)/g;
    var frame;
    while ((frame = keyframeRe.exec(source.text))) {
        var owners = keyframes.get(frame[1]) || [];
        owners.push(source.name);
        keyframes.set(frame[1], owners);
    }
});
var duplicates = [];
keyframes.forEach(function(owners, name) {
    if (owners.length > 1) duplicates.push(name + ': ' + owners.join(', '));
});
assert.deepStrictEqual(duplicates, [], 'animation keyframes must have one owner');

var tokens = sources[0].text;
assert(/--text-base:\s*0\.9375rem/.test(tokens), 'text tokens must scale from rem units');
assert(/--text-3xl:\s*1\.75rem/.test(tokens), 'the complete text scale must be defined');
assert(!/font-weight:\s*800\b/.test(allCss), 'CSS must not request an unloaded Outfit weight');
assert(!/--font-sans\s*:/.test(sources[10].text), 'Channels must inherit the app font contract');
assert(/\.nav-item\s*\{[^}]*min-height:\s*46px/s.test(sources[4].text),
    'navigation labels must grow instead of clipping scaled text');
var settingsFieldRule = sources[6].text.match(/\.view-grid-settings input\[type="text"\][\s\S]*?\.view-grid-settings \.modal-input\s*\{([^}]*)\}/);
assert(settingsFieldRule && /min-height:\s*34px/.test(settingsFieldRule[1]) && /height:\s*auto/.test(settingsFieldRule[1]),
    'settings fields must grow with scaled text');
assert(/data-text-scale-tier="xlarge"[\s\S]*?\.channels-layout/.test(sources[14].text),
    'the largest text tier must simplify the Channels layout');
assert(/data-text-scale-tier="xlarge"[\s\S]*?\.settings-text-scale-input/.test(sources[14].text),
    'the largest mobile text tier must give the slider its own row');

console.log('CSS contract tests passed');
