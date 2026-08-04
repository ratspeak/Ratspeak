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
    '00-palettes.css',
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
var sourceByName = Object.fromEntries(sources.map(function(source) {
    return [source.name, source.text];
}));
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

var tokens = sourceByName['00-tokens.css'];
assert(/--text-base:\s*0\.9375rem/.test(tokens), 'text tokens must scale from rem units');
assert(/--text-3xl:\s*1\.75rem/.test(tokens), 'the complete text scale must be defined');
assert(!/font-weight:\s*800\b/.test(allCss), 'CSS must not request an unloaded Outfit weight');
assert(!/--font-sans\s*:/.test(sourceByName['09-channels.css']), 'Channels must inherit the app font contract');
assert(/\.nav-item\s*\{[^}]*min-height:\s*46px/s.test(sourceByName['04-layout.css']),
    'navigation labels must grow instead of clipping scaled text');
var settingsFieldRule = sourceByName['06-forms.css'].match(/\.view-grid-settings input\[type="text"\][\s\S]*?\.view-grid-settings \.modal-input\s*\{([^}]*)\}/);
assert(settingsFieldRule && /min-height:\s*34px/.test(settingsFieldRule[1]) && /height:\s*auto/.test(settingsFieldRule[1]),
    'settings fields must grow with scaled text');
assert(/data-text-scale-tier="xlarge"[\s\S]*?\.channels-layout/.test(sourceByName['13-responsive.css']),
    'the largest text tier must simplify the Channels layout');
assert(/\.settings-type-presets\s*\{[\s\S]*?repeat\(5,/.test(sourceByName['10-views.css']),
    'text sizing must expose five deliberate presets instead of a continuous slider');
assert(/data-scale="100"[\s\S]*?--type-preview-size:\s*18px/.test(sourceByName['10-views.css']) &&
    /data-scale="140"[\s\S]*?--type-preview-size:\s*34px/.test(sourceByName['10-views.css']),
    'text-size specimens must make the preset progression visually distinct');
assert(/data-text-scale-tier="large"\] \.settings-text-scale-row \.settings-row-info\s*\{[^}]*flex:\s*0 0 auto/s.test(sourceByName['10-views.css']),
    'the 130% tier must not stretch the text-size introduction away from its presets');

var paletteCss = sourceByName['00-palettes.css'];
var themeSource = fs.readFileSync(path.join(dashboardRoot, 'static', 'js', 'theme.js'), 'utf8');
var themeFamilies = ['ratspeak', 'nord', 'everforest', 'gruvbox', 'catppuccin'];
var registeredFamilies = Array.from(themeSource.matchAll(/\bid:\s*'([^']+)'/g), function(entry) {
    return entry[1];
});
assert.deepStrictEqual(registeredFamilies, themeFamilies,
    'the appearance registry must expose the canonical ordered family set');

var requiredPaletteTokens = [
    '--theme-page-rgb', '--theme-ink-rgb', '--theme-border-rgb',
    '--theme-strong-border-rgb', '--theme-focus',
    '--bg-primary', '--bg-secondary', '--bg-tertiary', '--bg-card', '--bg-dark',
    '--border', '--border-light', '--border-subtle', '--border-card', '--border-control',
    '--text-primary', '--text-secondary', '--text-muted', '--text-disabled',
    '--accent', '--accent-dim', '--accent-dark', '--accent-light', '--accent-rgb', '--on-accent',
    '--status-online', '--status-online-fg', '--status-online-rgb',
    '--status-error', '--status-error-fg', '--status-error-rgb',
    '--status-warning', '--status-warning-fg', '--status-warning-rgb',
    '--status-info', '--status-info-fg', '--status-info-rgb',
    '--status-purple', '--status-purple-fg', '--status-purple-rgb',
    '--ble-accent', '--ble-accent-fg', '--ble-accent-rgb',
    '--surface-elevation-0', '--surface-elevation-1', '--surface-elevation-2',
    '--surface-elevation-3', '--surface-elevation-4', '--surface-elevation-5',
    '--chess-light', '--chess-dark', '--chess-border',
    '--chess-coord-light', '--chess-coord-dark',
    '--status-discovered', '--status-discovered-rgb', '--surface-game-gradient'
];

function paletteValue(body, token) {
    var match = body.match(new RegExp(token + '\\s*:\\s*([^;]+)\\s*;'));
    assert(match, 'missing value for ' + token);
    var value = match[1].trim();
    if (/^#[0-9A-Fa-f]{6}$/.test(value)) return value;
    var alias = value.match(/^var\((--[a-zA-Z0-9_-]+)\)$/);
    assert(alias, 'expected a hex value or direct alias for ' + token + ', got ' + value);
    return paletteValue(body, alias[1]);
}

function channel(value) {
    value /= 255;
    return value <= 0.04045 ? value / 12.92 : Math.pow((value + 0.055) / 1.055, 2.4);
}

function luminance(hex) {
    var value = parseInt(hex.slice(1), 16);
    return 0.2126 * channel((value >> 16) & 255) +
        0.7152 * channel((value >> 8) & 255) +
        0.0722 * channel(value & 255);
}

function contrast(a, b) {
    var first = luminance(a);
    var second = luminance(b);
    var lighter = Math.max(first, second);
    var darker = Math.min(first, second);
    return (lighter + 0.05) / (darker + 0.05);
}

function requireContrast(body, foreground, background, minimum, label) {
    var ratio = contrast(paletteValue(body, foreground), paletteValue(body, background));
    assert(ratio >= minimum, label + ' contrast is ' + ratio.toFixed(2) + ':1; expected ' + minimum + ':1');
}

function paletteBody(family, mode) {
    var source;
    var selector;
    if (family === 'ratspeak') {
        source = tokens;
        selector = mode === 'light' ? ':root {' : '[data-theme="dark"] {';
    } else {
        source = paletteCss;
        selector = ':root[data-theme-family="' + family + '"][data-theme="' + mode + '"]';
    }
    var start = source.indexOf(selector);
    assert(start >= 0, family + ' must define a ' + mode + ' palette');
    var bodyStart = source.indexOf('{', start);
    var bodyEnd = source.indexOf('\n}', bodyStart);
    return source.slice(bodyStart + 1, bodyEnd);
}

themeFamilies.forEach(function(family) {
    var familyStart = themeSource.indexOf("id: '" + family + "'");
    var nextFamilyStart = themeSource.indexOf("\n        {\n            id:", familyStart + 1);
    var familySource = themeSource.slice(
        familyStart,
        nextFamilyStart >= 0 ? nextFamilyStart : themeSource.indexOf('\n    ];', familyStart)
    );
    ['light', 'dark'].forEach(function(mode) {
        var body = paletteBody(family, mode);
        requiredPaletteTokens.forEach(function(token) {
            assert(new RegExp(token + '\\s*:').test(body),
                family + '/' + mode + ' is missing ' + token);
        });
        ['--text-primary', '--text-secondary', '--text-muted', '--accent', '--accent-dim'].forEach(function(token) {
            requireContrast(body, token, '--bg-primary', 4.5, family + '/' + mode + ' ' + token + ' on page');
            requireContrast(body, token, '--bg-card', 4.5, family + '/' + mode + ' ' + token + ' on panel');
        });
        requireContrast(body, '--on-accent', '--accent', 4.5,
            family + '/' + mode + ' accent foreground');
        requireContrast(body, '--on-accent', '--accent-dim', 4.5,
            family + '/' + mode + ' accent hover foreground');
        requireContrast(body, '--theme-focus', '--bg-primary', 3,
            family + '/' + mode + ' focus ring');
        requireContrast(body, '--border-control', '--bg-primary', 3,
            family + '/' + mode + ' control border on page');
        ['--status-online-fg', '--status-error-fg', '--status-warning-fg',
            '--status-info-fg', '--status-purple-fg', '--ble-accent-fg'].forEach(function(token) {
            requireContrast(body, token, '--bg-primary', 4.5,
                family + '/' + mode + ' ' + token + ' on page');
        });

        var previewMatch = familySource.match(new RegExp(
            mode + ": \\['(#[0-9A-Fa-f]{6})', '(#[0-9A-Fa-f]{6})', '(#[0-9A-Fa-f]{6})'\\]"
        ));
        assert(previewMatch, family + '/' + mode + ' must expose one picker preview triplet');
        assert.deepStrictEqual(
            previewMatch.slice(1).map(function(value) { return value.toUpperCase(); }),
            ['--bg-primary', '--bg-card', '--accent'].map(function(token) {
                return paletteValue(body, token).toUpperCase();
            }),
            family + '/' + mode + ' preview and native chrome colors must match its palette'
        );
    });
});

assert(/data-theme-family', family/.test(themeSource) &&
    /data-theme-preference', preference/.test(themeSource),
    'family, preference, and resolved mode must remain independent appearance axes');
assert(/ratspeak-theme-changed/.test(themeSource),
    'appearance changes must publish one shared frontend event');

console.log('CSS contract tests passed');
