#!/usr/bin/env node
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var dashboardRoot = path.join(__dirname, '..', '..', 'dashboard');
var toastJs = fs.readFileSync(path.join(dashboardRoot, 'static', 'js', 'toasts.js'), 'utf8');
var components = fs.readFileSync(path.join(dashboardRoot, 'static', 'css', '07-components.css'), 'utf8');
var kit = fs.readFileSync(path.join(dashboardRoot, 'static', 'css', '07-ui-components.css'), 'utf8');
var tokens = fs.readFileSync(path.join(dashboardRoot, 'static', 'css', '00-tokens.css'), 'utf8');
var animations = fs.readFileSync(path.join(dashboardRoot, 'static', 'css', '12-animations.css'), 'utf8');
var index = fs.readFileSync(path.join(dashboardRoot, 'index.html'), 'utf8');
var jsRoot = path.join(dashboardRoot, 'static', 'js');
var callsiteSources = fs.readdirSync(jsRoot)
    .filter(function(name) { return name.endsWith('.js') && name !== 'toasts.js'; })
    .map(function(name) {
        return { name: name, text: fs.readFileSync(path.join(jsRoot, name), 'utf8') };
    });

function callsiteSource(name) {
    var source = callsiteSources.find(function(entry) { return entry.name === name; });
    assert(source, 'missing reviewed toast source ' + name);
    return source.text;
}

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

// The kit toast (07-ui-components.css): a bare tone glyph, 12 px radius, bottom centre on desktop,
// above the tab bar on a phone. toasts.js never draws its own card.
assert(index.includes('<div id="toast-container" class="ui-toasts" role="status" aria-live="polite"></div>'),
    'toasts stack in the kit container, announced politely');
assert(toastJs.includes("toast.className = 'ui-toast' + kitTone + ' is-entering';"),
    'each toast is the kit toast, entering from below');
['toast-error', 'toast-warning', 'toast-success', 'toast-info', 'toast-progress', 'toast-action'].forEach(function(intent) {
    assert(toastJs.includes("'" + intent + "':"), 'toast API must accept the ' + intent + ' semantic intent');
});
assert(toastJs.includes("error: 'x-circle'") && toastJs.includes("warning: 'alert'") &&
    toastJs.includes("success: 'check-circle'") && toastJs.includes("info: 'info'") && toastJs.includes("action: 'inbox'"),
    'each tone has its bare kit glyph; something received reads as an inbox');
assert(toastJs.includes('<span class="ui-spin" aria-hidden="true"></span>'),
    'work in progress turns instead of showing a still glyph');
assert(toastJs.includes("msgSpan.className = 'msg'"),
    'toast copy must use the kit message slot');
assert(toastJs.includes("toast.setAttribute('role', 'alert')"),
    'error toasts must retain assertive announcement semantics');
assert(toastJs.includes("toast.setAttribute('aria-atomic', 'true')"),
    'each toast must be announced as one complete status update');
assert(toastJs.includes("var toastKey = message + '|' + tone"),
    'the visual refresh must preserve duplicate suppression');
assert(toastJs.includes('setTimeout(dismissToast, duration)'),
    'the visual refresh must preserve toast timing');
assert(toastJs.includes("actionBtn.className = 'toast-action-target'") &&
    toastJs.includes("actionBtn.setAttribute('aria-label', 'Open: ' + message)"),
    'actionable toasts must expose a native, named keyboard target');
assert(toastJs.includes("link.className = 'ui-link'"),
    'a named toast action is a kit Link');
assert(toastJs.includes("closeBtn.className = 'ui-icon-btn ui-icon-btn--sm'") &&
    toastJs.includes("closeBtn.setAttribute('aria-label', 'Dismiss')"),
    'dismissal is the kit icon button with its own label');
assert(toastJs.includes("direction: 'down'"),
    'toasts rise from the bottom bar, so a swipe down puts one away');
assert(!toastJs.includes("toast.style.cursor = 'pointer'"),
    'actionable semantics must not stop at a mouse-only cursor hint');
assert(/\.ui-toasts \{[^}]*bottom: 88px;[^}]*z-index: var\(--z-toast\);/.test(kit),
    'desktop toasts sit at the bottom centre, clear of pane headers and lifted above a composer row');
assert(/@media \(max-width: 768px\) \{ \.ui-toasts \{[^}]*bottom: calc\(76px \+ var\(--sab, 0px\)\);/.test(kit),
    'phone toasts sit above the tab bar and the home indicator');
assert(/\.ui-toast\.is-entering \{ opacity: 0; transform: translateY\(12px\); \}/.test(kit),
    'toasts enter by rising into place');
assert(/--z-toast: 100001;/.test(tokens) && /--z-overlay: 99999;/.test(tokens),
    'toasts stay above dialogs, so a copy inside one still confirms');
assert(/\.toast-action-target \{[^}]*position: absolute;[^}]*inset: 0;/.test(components),
    'the native action target must retain the whole-card tap area');
assert(/\.toast-action-target:focus-visible \{[^}]*outline: 2px solid var\(--focus-ring\)/.test(components),
    'actionable toasts must expose keyboard focus');
assert(!/#toast-container \{/.test(components) && !/\.toast \{/.test(components),
    'the pre-kit toast card is gone');
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

[
    'Voice message sent',
    'Challenge sent',
    'Delivery cancelled',
    'Channel session ended',
    'Channel notifications updated',
    'Hub settings saved',
    'Status saved',
    'Status cleared',
    'Conversation deleted',
    'Activity started',
    'Activity resumed',
    'Activity paused',
    'Requesting Bluetooth access',
    'Adding contact',
    'Disconnected from Offline Inbox node',
    'Offline Inbox announce queued',
    'Connecting to Offline Inbox node',
    'Checking Offline Inbox',
    'Switching channel hub',
    'Connecting to channel hub'
].forEach(function(redundantCopy) {
    assert(!toastCalls.some(function(call) { return call.source.includes(redundantCopy); }),
        'inline state must replace redundant toast: ' + redundantCopy);
});

var voiceSource = callsiteSource('voice_memos.js');
var lxmfSource = callsiteSource('lxmf.js');
assert(!extractCalls(voiceSource, 'showToast').some(function(call) {
    return /Voice message (?:sent|queued)/i.test(call);
}), 'voice send success must use the message row instead of a toast');
assert(lxmfSource.includes("content: 'Voice message'") &&
    lxmfSource.includes("state: resp && resp.cancelled ? 'cancelled' : 'sending'"),
    'outbound voice messages must enter the shared message delivery state machine');
assert(lxmfSource.includes("sent: ['Sent', '<polyline points=\"3 8.5 6.5 12 13 4.5\"/>']") &&
    lxmfSource.includes("delivered: ['Delivered', '<polyline") &&
    (lxmfSource.match(/_messageStateIconHtml\(/g) || []).length === 2,
    'voice and text messages must share the one set of checkmark delivery marks');
assert(!lxmfSource.includes("'New message from ' + escapeHtml(fromLabel)"),
    'toast textContent must receive the real contact name, not escaped markup text');
assert(lxmfSource.includes("? 'New voice message from ' + fromLabel"),
    'inbound voice messages must identify themselves without a redundant send toast');
assert(lxmfSource.includes('if (appForeground && !conversationVisible) {') &&
    lxmfSource.includes("!window.__TAURI_INTERNALS__ && !appForeground"),
    'messages must choose in-app or OS attention from actual foreground visibility');
var gamesSource = callsiteSource('games_tab.js');
assert(gamesSource.includes('if (appForeground) {') &&
    gamesSource.includes("!window.__TAURI_INTERNALS__ && !appForeground"),
    'game updates must not leave hidden in-app toasts alongside OS notifications');
assert(!callsiteSource('health.js').includes('Pausing interface…') &&
    !callsiteSource('health.js').includes('Resuming interface…'),
    'interface rows must own routine pause/resume progress');
assert(callsiteSource('tauri_events.js').includes(
    "if (data.error && !dialogResult) showToast(displayStep, 'toast-error', 5000)"
), 'interface lifecycle events must interrupt only on failure, and once: a waiting dialog shows its own');
assert(callsiteSource('constants.js').includes('toastErrorCopy(err, message)'),
    'generic command failures must apply the user/developer detail boundary');

var actionableCalls = toastCalls.filter(function(call) {
    var args = splitCallArguments(call.source);
    return args.length >= 4 && !!args[3];
});
assert(actionableCalls.length > 0, 'dashboard must retain actionable inbound toasts');
actionableCalls.forEach(function(call) {
    var args = splitCallArguments(call.source);
    if (args.length >= 5 && args[4]) {
        assert(args[1] === "'toast-error'" && args[4] === "'Copy details'",
            call.file + ' labelled toast actions are reserved for copying error details');
        return;
    }
    assert(call.source.includes("'toast-action'") || call.source.includes('"toast-action"'),
        call.file + ' callback toast must use the toast-action intent');
});

[
    'Resetting Ratspeak'
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
FakeElement.prototype.insertAdjacentHTML = function(_where, html) { this.children.push({ tagName: 'svg', html: html }); };

var toastContainer = new FakeElement('div');
var timers = [];
var sandbox = {
    window: {},
    document: {
        getElementById: function(id) { return id === 'toast-container' ? toastContainer : null; },
        createElement: function(tagName) { return new FakeElement(tagName); },
        createElementNS: function(_namespace, tagName) { return new FakeElement(tagName); }
    },
    RS: {
        gestures: {
            SWIPE_DISTANCE_TOAST_DISMISS_PX: 40,
            attachSwipe: function() {}
        },
        kit: { icon: function(name) { return '<svg data-icon="' + name + '"></svg>'; } }
    },
    requestAnimationFrame: function(callback) { callback(); },
    setTimeout: function(callback) { timers.push(callback); },
    Set: Set
};
vm.runInNewContext(toastJs, sandbox);
assert.strictEqual(
    sandbox.toastErrorCopy(
        { code: 'database_error', message: 'no such table: messages' },
        'Could not save contact'
    ),
    'Could not save contact',
    'normal product UI must not expose internal backend detail'
);
assert.strictEqual(
    sandbox.toastErrorCopy(
        { code: 'conflict', message: 'This contact is already saved' },
        'Could not save contact'
    ),
    'This contact is already saved',
    'reviewed corrective backend messages must remain useful to users'
);
sandbox.window.ratspeakDeveloperModeEnabled = function() { return true; };
assert.strictEqual(
    sandbox.toastErrorCopy(
        { code: 'database_error', message: 'no such table: messages' },
        'Could not save contact'
    ),
    'Could not save contact — no such table: messages',
    'developer mode may reveal bounded support detail'
);
var opened = 0;
sandbox.showToast('New message from Mountain Relay', 'toast-action', 4000, function() { opened += 1; });
var actionableToast = toastContainer.children[0];
assert.strictEqual(actionableToast.children[0].html, '<svg data-icon="inbox"></svg>',
    'an inbound action toast leads with the inbox glyph');
var actionTarget = actionableToast.children.find(function(child) {
    return child.className === 'toast-action-target';
});
assert(actionTarget && actionTarget.tagName === 'button',
    'actionable toast must render its whole-card action as a native button');
assert.strictEqual(actionTarget.attributes['aria-label'], 'Open: New message from Mountain Relay',
    'the action target must identify its destination when focused independently');
actionTarget.listeners.click();
assert.strictEqual(opened, 1, 'the native action target must preserve the toast callback');

var copied = 0;
sandbox.showToast("Ratspeak couldn't save some changes.", 'toast-error', 5000,
    function() { copied += 1; }, 'Copy details');
var errorToast = toastContainer.children[1];
assert.strictEqual(errorToast.attributes.role, 'alert', 'a labelled error toast stays an alert');
assert(/\bui-toast--error\b/.test(errorToast.className), 'an error toast takes the kit error tone');
var errorMessage = errorToast.children.find(function(child) { return child.className === 'msg'; });
assert.strictEqual(errorMessage.textContent, "Ratspeak couldn't save some changes.");
var copyLink = errorToast.children.find(function(child) { return child.className === 'ui-link'; });
assert(copyLink && copyLink.tagName === 'button' && copyLink.textContent === 'Copy details',
    'a labelled toast action is a visible Link button');
assert(!errorToast.children.some(function(child) { return child.className === 'toast-action-target'; }),
    'copying details never hides behind the whole card');
copyLink.listeners.click();
assert.strictEqual(copied, 1, 'the labelled action must run its callback');

var tauriEvents = callsiteSource('tauri_events.js');
assert(tauriEvents.includes("RS.listen('storage_error'") &&
    tauriEvents.includes('RS.copyText(details)') &&
    tauriEvents.includes("showCopyConfirmationToast('Details')") &&
    tauriEvents.includes("}, 'Copy details');"),
    'a failed write must offer its details on the clipboard');

console.log('toast design contract tests passed');
