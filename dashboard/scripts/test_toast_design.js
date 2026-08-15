#!/usr/bin/env node
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var dashboardRoot = path.join(__dirname, '..');
var toastJs = fs.readFileSync(path.join(dashboardRoot, 'static', 'js', 'toasts.js'), 'utf8');
var components = fs.readFileSync(path.join(dashboardRoot, 'static', 'css', '07-components.css'), 'utf8');
var responsive = fs.readFileSync(path.join(dashboardRoot, 'static', 'css', '13-responsive.css'), 'utf8');
var animations = fs.readFileSync(path.join(dashboardRoot, 'static', 'css', '12-animations.css'), 'utf8');
var index = fs.readFileSync(path.join(dashboardRoot, 'index.html'), 'utf8');
var jsRoot = path.join(dashboardRoot, 'static', 'js');
var callsiteSources = fs.readdirSync(jsRoot)
    .filter(function(name) { return name.endsWith('.js') && name !== 'toasts.js'; })
    .map(function(name) {
        return { name: name, text: fs.readFileSync(path.join(jsRoot, name), 'utf8') };
    });

function extractCalls(source, functionName) {
    var calls = [];
    var matcher = new RegExp('\\b' + functionName + '\\s*\\(', 'g');
    var match;
    while ((match = matcher.exec(source))) {
        var start = match.index;
        var open = source.indexOf('(', start);
        var depth = 0;
        var quote = '';
        var escaped = false;
        var end = -1;
        for (var i = open; i < source.length; i += 1) {
            var ch = source[i];
            var next = source[i + 1];
            if (quote) {
                if (escaped) {
                    escaped = false;
                } else if (ch === '\\') {
                    escaped = true;
                } else if (ch === quote) {
                    quote = '';
                }
                continue;
            }
            if (ch === '/' && next === '/') {
                i = source.indexOf('\n', i + 2);
                if (i === -1) break;
                continue;
            }
            if (ch === '/' && next === '*') {
                i = source.indexOf('*/', i + 2);
                if (i === -1) break;
                i += 1;
                continue;
            }
            if (ch === '\'' || ch === '"' || ch === '`') {
                quote = ch;
            } else if (ch === '(') {
                depth += 1;
            } else if (ch === ')') {
                depth -= 1;
                if (depth === 0) {
                    end = i + 1;
                    break;
                }
            }
        }
        assert(end !== -1, 'could not parse ' + functionName + ' call near source offset ' + start);
        calls.push(source.slice(start, end));
        matcher.lastIndex = end;
    }
    return calls;
}

function splitCallArguments(call) {
    var body = call.slice(call.indexOf('(') + 1, -1);
    var args = [];
    var start = 0;
    var round = 0;
    var square = 0;
    var curly = 0;
    var quote = '';
    var escaped = false;
    for (var i = 0; i < body.length; i += 1) {
        var ch = body[i];
        var next = body[i + 1];
        if (quote) {
            if (escaped) {
                escaped = false;
            } else if (ch === '\\') {
                escaped = true;
            } else if (ch === quote) {
                quote = '';
            }
            continue;
        }
        if (ch === '/' && next === '/') {
            i = body.indexOf('\n', i + 2);
            if (i === -1) break;
            continue;
        }
        if (ch === '/' && next === '*') {
            i = body.indexOf('*/', i + 2);
            if (i === -1) break;
            i += 1;
            continue;
        }
        if (ch === '\'' || ch === '"' || ch === '`') {
            quote = ch;
        } else if (ch === '(') {
            round += 1;
        } else if (ch === ')') {
            round -= 1;
        } else if (ch === '[') {
            square += 1;
        } else if (ch === ']') {
            square -= 1;
        } else if (ch === '{') {
            curly += 1;
        } else if (ch === '}') {
            curly -= 1;
        } else if (ch === ',' && round === 0 && square === 0 && curly === 0) {
            args.push(body.slice(start, i).trim());
            start = i + 1;
        }
    }
    args.push(body.slice(start).trim());
    return args;
}

var toastCalls = [];
callsiteSources.forEach(function(source) {
    extractCalls(source.text, 'showToast').forEach(function(call) {
        toastCalls.push({ file: source.name, source: call });
    });
});

assert(toastJs.includes("status.className = 'toast-status'"),
    'toasts must carry a compact semantic status mark');
assert(toastJs.includes("status.setAttribute('aria-hidden', 'true')"),
    'the decorative status mark must stay out of the accessibility tree');
['toast-error', 'toast-warning', 'toast-success', 'toast-info', 'toast-progress', 'toast-action'].forEach(function(intent) {
    assert(toastJs.includes("'" + intent + "':"), 'toast API must accept the ' + intent + ' semantic intent');
});
assert(toastJs.includes("colorClass === 'toast-progress'") &&
    toastJs.includes("colorClass === 'toast-action'"),
    'progress and actionable notices must have distinct status glyphs');
assert(toastJs.includes("msgSpan.className = 'toast-message'"),
    'toast copy must use the shared presentation class');
assert(toastJs.includes("toast.setAttribute('role', 'alert')"),
    'error toasts must retain assertive announcement semantics');
assert(toastJs.includes("toast.setAttribute('aria-atomic', 'true')"),
    'each toast must be announced as one complete status update');
assert(toastJs.includes("var toastKey = message + '|' + colorClass"),
    'the visual refresh must preserve duplicate suppression');
assert(toastJs.includes('setTimeout(dismissToast, duration)'),
    'the visual refresh must preserve toast timing');
assert(toastJs.includes("actionBtn.className = 'toast-action-target'") &&
    toastJs.includes("actionBtn.setAttribute('aria-label', 'Open: ' + message)"),
    'actionable toasts must expose a native, named keyboard target');
assert(!toastJs.includes("toast.style.cursor = 'pointer'"),
    'actionable semantics must not stop at a mouse-only cursor hint');
assert(toastJs.includes("svg.setAttribute('aria-hidden', 'true')") &&
    toastJs.includes("svg.setAttribute('focusable', 'false')"),
    'the dismiss glyph must not duplicate its button label');

var toastRule = components.match(/\.toast\s*\{([\s\S]*?)\n\}/);
assert(toastRule && /grid-template-columns:\s*28px minmax\(0, 1fr\) 32px/.test(toastRule[1]),
    'toast layout must give status, message, and dismissal deliberate columns');
assert(toastRule && /background:\s*var\(--surface-popover\)/.test(toastRule[1]),
    'toast cards must use the active theme surface');
assert(!toastRule[1].includes('border-left'),
    'toast cards must not return to the colored left-rail treatment');
assert(!/\.toast\.toast-(?:blue|green|red|orange|purple)[^{]*\{[^}]*linear-gradient/s.test(components),
    'toast variants must not wash the entire card in a status gradient');
assert(/\.toast\.toast-yellow\s*\{[^}]*--toast-tone:\s*var\(--status-warning-fg\)/s.test(components),
    'the existing yellow warning alias must receive the warning presentation');
assert(/\.toast\.toast-progress\s*\{[^}]*--toast-tone:\s*var\(--status-info-fg\)/s.test(components),
    'progress toasts must use the informational status family');
assert(/\.toast\.toast-action\s*\{[^}]*--toast-tone:\s*var\(--accent\)/s.test(components),
    'actionable inbound toasts must use the Ratspeak action accent');
assert(/\.toast-close:focus-visible\s*\{[^}]*outline:\s*2px solid var\(--focus-ring\)/s.test(components),
    'toast dismissal must expose keyboard focus');
assert(/\.toast-action-target\s*\{[^}]*position:\s*absolute;[^}]*inset:\s*0;/s.test(components),
    'the native action target must retain the whole-card tap area');
assert(/\.toast-action-target:focus-visible\s*\{[^}]*outline:\s*2px solid var\(--focus-ring\)/s.test(components),
    'actionable toasts must expose keyboard focus');
assert(!components.includes('.toast-actionable .toast-message::after'),
    'the action glyph must remain the single affordance instead of repeating a chevron');
assert(/@media \(max-width:\s*768px\)[\s\S]*?#toast-container[\s\S]*?top:\s*calc\(8px \+ var\(--sat\)\)/.test(responsive),
    'mobile toasts must respect the device safe area');
assert(/@media \(max-width:\s*768px\)[\s\S]*?\.toast\s*\{[^}]*grid-template-columns:\s*28px minmax\(0, 1fr\) 44px/s.test(responsive),
    'the mobile toast grid must reserve the complete dismissal touch target');
assert(/@media \(prefers-reduced-motion:\s*reduce\)[\s\S]*?transition-duration:\s*0\.01ms !important/.test(animations),
    'toast motion must inherit the global reduced-motion contract');
var styleRevision = index.match(/\/static\/style\.css\?v=([^"']+)/);
var toastRevision = index.match(/\/static\/js\/toasts\.js\?v=([^"']+)/);
assert(styleRevision && toastRevision && styleRevision[1] === toastRevision[1],
    'toast presentation assets must share the dashboard build revision');

var allowedIntents = [
    'toast-action', 'toast-error', 'toast-info',
    'toast-progress', 'toast-success', 'toast-warning'
];
var observedIntents = new Set();
toastCalls.forEach(function(call) {
    var intentPattern = /(['"])(toast-[a-z-]+)\1/g;
    var intentMatch;
    while ((intentMatch = intentPattern.exec(call.source))) {
        assert(allowedIntents.includes(intentMatch[2]),
            call.file + ' uses unsupported or legacy toast intent ' + intentMatch[2]);
        observedIntents.add(intentMatch[2]);
    }
    assert(!/,\s*(['"])\1\s*,/.test(call.source),
        call.file + ' must not use an empty toast intent');
});
assert.deepStrictEqual(Array.from(observedIntents).sort(), allowedIntents,
    'dashboard callsites must exercise exactly the six semantic toast intents');

var callsiteText = callsiteSources.map(function(source) { return source.text; }).join('\n');
[
    'Services ready',
    'QR handed to destination',
    'network blackhole',
    'Message exceeds protocol limit',
    'File exceeds protocol limit',
    'Saved to photos!',
    '\\uD83C\\uDFAE',
    '🎮'
].forEach(function(stalePhrase) {
    assert(!callsiteText.includes(stalePhrase),
        'dashboard toast copy must not restore stale phrase: ' + stalePhrase);
});

var actionableCalls = toastCalls.filter(function(call) {
    var args = splitCallArguments(call.source);
    return args.length >= 4 && !!args[3];
});
assert(actionableCalls.length > 0, 'dashboard must retain actionable inbound toasts');
actionableCalls.forEach(function(call) {
    assert(call.source.includes("'toast-action'") || call.source.includes('"toast-action"'),
        call.file + ' callback toast must use the toast-action intent');
});

[
    'Requesting Bluetooth access',
    'Resetting Ratspeak',
    'Adding contact',
    'Pausing interface',
    'Resuming interface',
    'Connecting to Offline Inbox node',
    'Switching channel hub',
    'Connecting to channel hub',
    'Checking Offline Inbox'
].forEach(function(progressCopy) {
    var matchingCalls = toastCalls.filter(function(call) { return call.source.includes(progressCopy); });
    assert(matchingCalls.length > 0, 'missing reviewed pending-operation toast: ' + progressCopy);
    matchingCalls.forEach(function(call) {
        assert(call.source.includes("'toast-progress'") || call.source.includes('"toast-progress"'),
            call.file + ' pending-operation toast must use toast-progress: ' + progressCopy);
    });
});

function FakeElement(tagName) {
    this.tagName = tagName;
    this.children = [];
    this.attributes = {};
    this.listeners = {};
    this.className = '';
    this.style = {};
    this.classList = {
        add: function() {},
        remove: function() {}
    };
}
FakeElement.prototype.setAttribute = function(name, value) { this.attributes[name] = value; };
FakeElement.prototype.appendChild = function(child) { this.children.push(child); return child; };
FakeElement.prototype.addEventListener = function(name, handler) { this.listeners[name] = handler; };
FakeElement.prototype.remove = function() { this.removed = true; };

var toastContainer = new FakeElement('div');
var timers = [];
var sandbox = {
    document: {
        getElementById: function(id) { return id === 'toast-container' ? toastContainer : null; },
        createElement: function(tagName) { return new FakeElement(tagName); },
        createElementNS: function(_namespace, tagName) { return new FakeElement(tagName); }
    },
    RS: {
        gestures: {
            SWIPE_DISTANCE_TOAST_DISMISS_PX: 40,
            attachSwipe: function() {}
        }
    },
    requestAnimationFrame: function(callback) { callback(); },
    setTimeout: function(callback) { timers.push(callback); },
    Set: Set
};
vm.runInNewContext(toastJs, sandbox);
var opened = 0;
sandbox.showToast('New message from Mountain Relay', 'toast-action', 4000, function() { opened += 1; });
var actionableToast = toastContainer.children[0];
var actionTarget = actionableToast.children.find(function(child) {
    return child.className === 'toast-action-target';
});
assert(actionTarget && actionTarget.tagName === 'button',
    'actionable toast must render its whole-card action as a native button');
assert.strictEqual(actionTarget.attributes['aria-label'], 'Open: New message from Mountain Relay',
    'the action target must identify its destination when focused independently');
actionTarget.listeners.click();
assert.strictEqual(opened, 1, 'the native action target must preserve the toast callback');

console.log('toast design contract tests passed');
