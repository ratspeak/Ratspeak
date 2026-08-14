#!/usr/bin/env node
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var dashboardRoot = path.join(__dirname, '..');
function read(relative) {
    return fs.readFileSync(path.join(dashboardRoot, relative), 'utf8');
}
function functionSource(source, name) {
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

var stateSource = read('static/js/state.js');
var breakpointContext = {
    window: { innerWidth: 768, RS: { config: { MOBILE_BREAKPOINT: 768, MOBILE_TOUCH_BREAKPOINT: 1024 } } },
    navigator: { maxTouchPoints: 0 }
};
vm.createContext(breakpointContext);
vm.runInContext(
    functionSource(stateSource, 'isCompactLayout') + '\n' +
    functionSource(stateSource, 'isTouchDevice') + '\n' +
    functionSource(stateSource, 'isMobile'),
    breakpointContext
);
assert.strictEqual(breakpointContext.isCompactLayout(), true);
breakpointContext.window.innerWidth = 769;
assert.strictEqual(breakpointContext.isCompactLayout(), false);
breakpointContext.navigator.maxTouchPoints = 1;
breakpointContext.window.innerWidth = 1024;
assert.strictEqual(breakpointContext.isMobile(), true);
breakpointContext.window.innerWidth = 1025;
assert.strictEqual(breakpointContext.isMobile(), false);

var storage = {};
var root = { style: {}, dataset: {} };
function CustomEvent(name, options) { this.type = name; this.detail = options.detail; }
var scaleContext = {
    Number: Number,
    Math: Math,
    CustomEvent: CustomEvent,
    localStorage: {
        getItem: function(key) { return storage[key] || null; },
        setItem: function(key, value) { storage[key] = value; },
        removeItem: function(key) { delete storage[key]; }
    },
    document: { documentElement: root },
    window: { CustomEvent: CustomEvent, dispatchEvent: function(event) { this.lastEvent = event; } }
};
scaleContext.window.window = scaleContext.window;
scaleContext.window.document = scaleContext.document;
scaleContext.window.localStorage = scaleContext.localStorage;
vm.createContext(scaleContext);
vm.runInContext(read('static/js/text_scale.js'), scaleContext);
assert.strictEqual(scaleContext.window.RS.textScale.get(), 100);
assert.strictEqual(scaleContext.window.RS.textScale.commit(127), 130);
assert.strictEqual(root.style.fontSize, '130%');
assert.strictEqual(root.dataset.textScaleTier, 'large');
assert.strictEqual(storage['rs-text-scale-percent'], '130');
assert.strictEqual(scaleContext.window.RS.textScale.commit(200), 140);
assert.strictEqual(root.dataset.textScaleTier, 'xlarge');
assert.strictEqual(scaleContext.window.RS.textScale.reset(), 100);
assert.strictEqual(storage['rs-text-scale-percent'], undefined);

var html = read('index.html');
var ids = Array.from(html.matchAll(/\sid="([^"]+)"/g), function(match) { return match[1]; });
var duplicateIds = Array.from(new Set(ids.filter(function(id, index) {
    return ids.indexOf(id) !== index;
})));
assert.deepStrictEqual(duplicateIds, [], 'dashboard markup must not contain duplicate ids');
var ariaReferences = Array.from(html.matchAll(/\saria-(?:labelledby|describedby)="([^"]+)"/g))
    .reduce(function(references, match) {
        return references.concat(match[1].trim().split(/\s+/));
    }, []);
assert.deepStrictEqual(Array.from(new Set(ariaReferences.filter(function(id) {
    return ids.indexOf(id) === -1;
}))), [], 'ARIA references must resolve to dashboard elements');
var labelTargets = Array.from(html.matchAll(/<label\b[^>]*\sfor="([^"]+)"/g), function(match) {
    return match[1];
});
assert.deepStrictEqual(Array.from(new Set(labelTargets.filter(function(id) {
    return ids.indexOf(id) === -1;
}))), [], 'label for attributes must resolve to dashboard controls');
assert.strictEqual((html.match(/name="settings-text-scale"/g) || []).length, 5);
['100', '110', '120', '130', '140'].forEach(function(value) {
    assert(html.includes('name="settings-text-scale" value="' + value + '"'));
});
assert(html.includes('class="settings-type-presets"'));
assert(html.includes('aria-labelledby="settings-text-scale-label"'));
assert(html.includes('aria-describedby="settings-text-scale-desc"'));
assert(!html.includes('class="settings-detail-eyebrow"'),
    'settings detail headers must not repeat the Ratspeak product name');
assert(!html.includes('identity-page-kicker'),
    'Identity Management must not repeat an ornamental Identity eyebrow');
assert(!/<legend\b[^>]*>\s*Text size\s*<\/legend>/.test(html),
    'the visible settings label must be the preset group\'s only text-size heading');
assert(!html.includes('no_pinch.js'), 'browser zoom must remain available');
assert(html.includes('lxmf-compose message-composer'));
assert(html.includes('channel-compose message-composer'));
assert(html.includes('message-composer-input'));
assert(html.includes('message-send-btn'));
assert(html.includes('games-sidebar-tabs section-subtabs'));
assert(html.includes('games-tab section-subtab-btn'));
assert(html.includes('network-subtabs section-subtabs'));
assert(html.includes('network-subtab-btn section-subtab-btn'));
assert(!/<a class="bottom-sheet-item"/.test(html), 'mobile navigation rows must be buttons');
assert(!/<a class="bottom-bar-item"/.test(html), 'mobile primary navigation must use semantic buttons');

var revisionMatches = Array.from(html.matchAll(/\/(?:static\/(?!js\/vendor)[^"?]+)\?v=([^"&]+)/g));
assert(revisionMatches.length > 20, 'first-party asset revisions must be explicit');
var revisions = new Set(revisionMatches.map(function(match) { return match[1]; }));
assert.deepStrictEqual(Array.from(revisions), ['ui-20260804'],
    'first-party CSS, fonts, and JS must share one build-level asset revision');

var nav = read('static/js/nav.js');
assert(nav.includes("document.querySelectorAll('#bottom-sheet .bottom-sheet-item[data-view]')"));
assert(nav.includes('sheet._ratspeakDismiss = close;'));
assert(nav.includes('var initialView = _resolveInitialView();'));
assert(nav.includes('switchView(initialView);'));
var routeContext = {
    VIEWS: ['dashboard', 'peers', 'message', 'channels', 'network', 'settings'],
    VIEW_ALIASES: { eventlog: 'network', propagation: 'network' },
    window: { location: { hash: '#eventlog' } },
    localStorage: { getItem: function() { return 'propagation'; } },
    isCompactLayout: function() { return true; },
    String: String
};
vm.createContext(routeContext);
vm.runInContext(
    functionSource(nav, '_normalizeViewId') + '\n' +
    functionSource(nav, '_resolveInitialView'),
    routeContext
);
assert.strictEqual(routeContext._resolveInitialView(), 'network',
    'a normalized explicit hash wins on compact layouts');
routeContext.window.location.hash = '#unknown';
assert.strictEqual(routeContext._resolveInitialView(), 'peers',
    'compact layouts use Peers when no valid deep link exists');
routeContext.isCompactLayout = function() { return false; };
assert.strictEqual(routeContext._resolveInitialView(), 'network',
    'wide layouts normalize the saved route before falling back');
var setupSource = read('static/js/setup.js');
var setupRouteContext = { isCompactLayout: function() { return true; } };
vm.createContext(setupRouteContext);
vm.runInContext(functionSource(setupSource, 'setupCompletionView'), setupRouteContext);
assert.strictEqual(setupRouteContext.setupCompletionView(), 'peers',
    'fresh compact setups must enter the real Peers view');
setupRouteContext.isCompactLayout = function() { return false; };
assert.strictEqual(setupRouteContext.setupCompletionView(), 'dashboard',
    'fresh wide setups retain the desktop Dashboard landing view');
assert(setupSource.includes("window.location.href = '/#' + setupCompletionView()"),
    'setup completion must route through the responsive landing-view policy');
assert(!setupSource.includes("window.location.href = '/#dashboard'"),
    'setup completion must not force the desktop-only Dashboard route');
var shared = read('static/js/ui_shared.js');
assert(shared.includes('modal._ratspeakDismiss = function()'));
assert(shared.includes('RS.composer.resize = function'));
assert(shared.includes('RS.text.utf8Length = function'));
assert(shared.includes('RS.ui.bindHelpPopovers = function'));
assert(shared.includes('RS.ui.prefersKeyboardFocus = function'));
assert(!read('static/js/setup.js').includes('tooltip-backdrop'),
    'setup help must not dim the entire screen with a bespoke backdrop');
var channelsSource = read('static/js/channels.js');
assert(channelsSource.includes('channel-consent-acknowledgements'),
    'public-channel safety confirmations must remain one coherent acknowledgement group');
var legalDocumentsSource = read('static/js/legal_documents.js');
assert(channelsSource.includes('RS.legal.open(documentId)') &&
    legalDocumentsSource.includes("version: '2026-08-11'") &&
    legalDocumentsSource.includes('Available offline'),
    'public-channel policies must open the versioned offline reader');
assert(legalDocumentsSource.includes('View current version online') &&
    legalDocumentsSource.includes('Promise.resolve(RS.openExternalUrl(current.url))'),
    'the offline reader must retain an explicit, failure-aware path to the current online copy');
assert(channelsSource.includes('RS.ui.focusAfterUpdate(initialFocus)'),
    'compact channel sheets must not force keyboard focus during touch navigation');
var lxmf = read('static/js/lxmf.js');
assert(lxmf.includes("e.key === 'Enter' && !e.shiftKey && !e.isComposing && !isMobile()"));
assert(!read('static/js/peers.js').includes('displayName.substring(0, 40)'));
assert(!read('static/js/connections.js').includes('displayName.substring(0, 40)'));
assert(stateSource.includes("return '<button class=\"hash-copy\" type=\"button\""),
    'compact hashes must remain keyboard-operable copy buttons');
assert(read('static/js/peers.js').includes('<button class="peers-detail-hash"'),
    'peer detail hashes must use a semantic copy control');

var identitySource = read('static/js/identity.js');
var identityContext = {
    escapeHtml: function(value) {
        return String(value || '').replace(/&/g, '&amp;').replace(/</g, '&lt;')
            .replace(/>/g, '&gt;').replace(/"/g, '&quot;').replace(/'/g, '&#39;');
    }
};
vm.createContext(identityContext);
vm.runInContext(functionSource(identitySource, 'identityAddressRowHtml'), identityContext);
var sampleIdentityHash = '71e7e82e83b19c54bef56903aacf5d7f';
var addressRow = identityContext.identityAddressRowHtml('LXMF Address', sampleIdentityHash);
assert.strictEqual((addressRow.match(/<button\b/g) || []).length, 1,
    'an identity address row must be one copy button without nested interactive controls');
assert(addressRow.includes('data-copy-value="' + sampleIdentityHash + '"'));
assert(addressRow.includes('<span class="identity-value mono" dir="ltr">' + sampleIdentityHash + '</span>'));
assert(addressRow.includes('class="identity-address-action" aria-hidden="true"'));
assert(!identitySource.includes('copyableHash(lxmfHash)') &&
    !identitySource.includes('copyableHash(identityHash)'),
    'the full-row copy controls must render plain hash text instead of nested hash buttons');

console.log('Frontend foundation tests passed');
