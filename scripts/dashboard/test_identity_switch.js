#!/usr/bin/env node
'use strict';

// A switch that does not finish ends with identity_switch_failed: the window
// leaves its switching state, whatever the generation, and says why.

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var root = path.join(__dirname, '..', '..');
var identity = fs.readFileSync(path.join(root, 'dashboard/static/js/identity.js'), 'utf8');
var lxmf = fs.readFileSync(path.join(root, 'dashboard/static/js/lxmf.js'), 'utf8');
var setup = fs.readFileSync(path.join(root, 'dashboard/static/js/setup.js'), 'utf8');

function block(source, start, what) {
    assert(start !== -1, 'missing ' + what);
    var depth = 0;
    for (var i = source.indexOf('{', start); i < source.length; i++) {
        if (source[i] === '{') depth++;
        else if (source[i] === '}' && --depth === 0) return source.slice(start, i + 1);
    }
    throw new Error('unterminated ' + what);
}

function functionSource(name, source) {
    source = source || identity;
    return block(source, source.indexOf('function ' + name + '('), name);
}

function listenerSource(event, source) {
    source = source || identity;
    var start = source.indexOf("RS.listen('" + event + "', function(");
    var body = block(source, start, 'listener ' + event);
    return body + source.slice(start + body.length, source.indexOf(';', start + body.length) + 1);
}

function settle() {
    return new Promise(function(resolve) { setImmediate(resolve); });
}

var handlers = {};
var calls = [];
var toasts = [];
var fail = null;
var card = { innerHTML: '' };
var page = {
    window: {},
    document: {
        getElementById: function(id) { return id === 'identity-active-card' ? card : null; },
    },
    RS: {
        listen: function(event, handler) { handlers[event] = handler; },
        invoke: function(name) {
            calls.push(name);
            return new Promise(function(_resolve, reject) { fail = reject; });
        },
    },
    showToast: function(message, kind) { toasts.push([message, kind]); },
    loadIdentities: function() { calls.push('loadIdentities'); },
    loadConversations: function() { calls.push('loadConversations'); },
    setTimeout: function() {},
    normalizeContactList: function(data) { return data.slice(); },
    _refreshRenderedConversationNames: function() {},
    renderVoiceUi: function() {},
};
vm.createContext(page);
var pendingDeclaration = identity.slice(identity.indexOf('var _switchRequestsPending'),
    identity.indexOf(';', identity.indexOf('var _switchRequestsPending')) + 1);
assert(pendingDeclaration, 'missing the pending-switch count');
page.renderIdentityPage = function() { calls.push('renderIdentityPage'); };
page.activeIdentity = function() { return { display_name: 'A' }; };
page.identityDisplayName = function(ident) { return ident.display_name; };
page.isViewActive = function() { return false; };
vm.runInContext(pendingDeclaration + '\nvar _identitySwitchingHash = null, _identitySwitchFailed = null;\n' +
    functionSource('switchToIdentity') + '\n' + functionSource('_identityNoteSwitchFailure') + '\n' +
    functionSource('resetConversationCaches') + '\n' +
    listenerSource('identity_switching') + '\n' +
    listenerSource('identity_switched') + '\n' +
    listenerSource('identity_switch_failed') + '\n' +
    listenerSource('contacts_update', lxmf), page, { filename: 'identity-switch.js' });

// Setup's security key activation fails after the previous runtime stopped:
// its reply and the switch's closing event carry one message, in either
// order, and the real toast shows it once.
async function setupFailureIsShownOnce() {
    var toastsSource = fs.readFileSync(path.join(root, 'dashboard/static/js/toasts.js'), 'utf8');
    for (var order of ['event first', 'reply first']) {
        var shown = [];
        function element() {
            return {
                style: {}, classList: { add: function() {}, remove: function() {} },
                setAttribute: function() {}, addEventListener: function() {}, remove: function() {},
                appendChild: function(child) { return child; }, insertAdjacentHTML: function() {},
            };
        }
        var container = element();
        container.appendChild = function(child) { shown.push(child); return child; };
        var reply = null;
        var listeners = {};
        var screen = {
            window: {},
            document: {
                createElement: element,
                createElementNS: element,
                getElementById: function(id) { return id === 'toast-container' ? container : null; },
            },
            requestAnimationFrame: function() {},
            setTimeout: function() {},
            RS: {
                listen: function(event, handler) { listeners[event] = handler; },
                invoke: function() { return new Promise(function(_resolve, reject) { reply = reject; }); },
                gestures: { attachSwipe: function() {}, SWIPE_DISTANCE_TOAST_DISMISS_PX: 0 },
                kit: { icon: function() { return ''; } },
                diag: function() {},
            },
            loadIdentities: function() {},
            showSetupConnectingStep: function() {},
            resetSetupToStart: function() {},
        };
        screen.window.RS = screen.RS;
        vm.createContext(screen);
        vm.runInContext(
            toastsSource.slice(0, toastsSource.indexOf('function toastDeveloperDetailsEnabled')) +
            functionSource('showToast', toastsSource) + '\n' +
            pendingDeclaration + '\n' +
            'var _identitySwitchFailed = null; function activeIdentity() { return null; } function isViewActive() { return false; }\n' +
            functionSource('_identityNoteSwitchFailure') + '\n' +
            listenerSource('identity_switch_failed') + '\n' +
            functionSource('completeSetupAfterHardwareIdentity', setup),
            screen,
            { filename: 'setup-failure.js' }
        );
        var hash = 'cc'.repeat(16);
        var message = 'Could not activate hardware identity.';
        screen.completeSetupAfterHardwareIdentity({ hash: hash }, '123456');
        var failed = new Error(message);
        failed.code = 'internal_error';
        if (order === 'event first') {
            listeners.identity_switch_failed({ hash: hash, generation: '12', error: message });
            reply(failed);
            await settle();
        } else {
            reply(failed);
            await settle();
            listeners.identity_switch_failed({ hash: hash, generation: '12', error: message });
        }
        assert.strictEqual(shown.length, 1, order + ': the failure is shown ' + shown.length + ' times');
    }
}

(async function() {
    var target = 'bb'.repeat(16);
    var reason = "Couldn't load this identity's key.";

    // The previous identity's contacts leave the window when a switch
    // starts, not when it ends.
    page.lxmfContacts = [{ hash: 'cc'.repeat(16), display_name: 'Previous' }];
    var rendered = 0;
    page.renderStandaloneContactList = function() { rendered += 1; };
    handlers.identity_switching({ hash: target, generation: '8' });
    assert.strictEqual(page.lxmfContacts.length, 0, 'the previous contacts stay while switching');
    assert.strictEqual(rendered, 1, 'the contact list is not redrawn');
    // The next identity's start sends its contacts before identity_switched,
    // which must keep them.
    var next = 'dd'.repeat(16);
    handlers.contacts_update([{ hash: next, display_name: 'Next' }]);
    handlers.identity_switched({ hash: target, lxmf_hash: '', display_name: 'B' });
    assert.strictEqual(page.lxmfContacts.length, 1, 'identity_switched dropped the new contacts');
    assert.strictEqual(page.lxmfContacts[0].hash, next);
    page.window._identitySwitchInProgress = false;
    toasts = [];
    calls = [];

    // The switch began and failed: its closing event ends it.
    page.switchToIdentity(target);
    assert.strictEqual(page._identitySwitchingHash, target, 'the detail shows the switch in its button');
    page.window._identitySwitchInProgress = true;
    calls = [];
    handlers.identity_switch_failed({ hash: target, generation: '9', error: reason });
    assert.strictEqual(page.window._identitySwitchInProgress, false, 'the window stays mid-switch');
    assert(calls.indexOf('loadIdentities') !== -1, 'the switching overlay stays');
    assert(calls.indexOf('loadConversations') !== -1, 'the restored identity is not reloaded');
    var error = new Error(reason);
    error.code = 'bad_request';
    fail(error);
    await settle();
    assert.deepStrictEqual(toasts, [[reason, 'toast-error']], 'the reason is swallowed');

    // Started elsewhere, by a removal's re-activation or another window: the
    // event is all this window hears, so it says why.
    toasts = [];
    calls = [];
    handlers.identity_switch_failed({ hash: target, generation: '10', error: reason });
    assert.deepStrictEqual(toasts, [[reason, 'toast-error']], 'a switch this window did not start fails silently');

    // Refused before it began: no event, so the reply clears the overlay.
    toasts = [];
    calls = [];
    page.switchToIdentity(target);
    var refused = new Error('Identity not found');
    refused.code = 'not_found';
    fail(refused);
    await settle();
    assert.deepStrictEqual(toasts, [['Identity not found', 'toast-error']]);
    assert(calls.indexOf('loadIdentities') !== -1, 'a refused switch leaves its overlay');

    await setupFailureIsShownOnce();
    console.log('Identity switch tests passed');
})().catch(function(error) {
    console.error(error);
    process.exit(1);
});
