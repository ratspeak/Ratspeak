#!/usr/bin/env node
// Behavioral coverage for message actions and elevated native text selection.

'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var dashboardRoot = path.join(__dirname, '..');
var lxmfSource = fs.readFileSync(path.join(dashboardRoot, 'static', 'js', 'lxmf.js'), 'utf8');
var gesturesSource = fs.readFileSync(path.join(dashboardRoot, 'static', 'js', 'gestures.js'), 'utf8');
var messagingCss = fs.readFileSync(path.join(dashboardRoot, 'static', 'css', '09-messaging.css'), 'utf8');

function namedFunctionSource(source, name) {
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

function EventTargetStub() {
    this.listeners = {};
}
EventTargetStub.prototype.addEventListener = function(type, listener) {
    (this.listeners[type] = this.listeners[type] || []).push(listener);
};
EventTargetStub.prototype.removeEventListener = function(type, listener) {
    var listeners = this.listeners[type] || [];
    this.listeners[type] = listeners.filter(function(candidate) { return candidate !== listener; });
};
EventTargetStub.prototype.dispatch = function(type, event) {
    (this.listeners[type] || []).slice().forEach(function(listener) { listener(event || {}); });
};

function targetWithClasses(classes) {
    var own = new Set(classes || []);
    return {
        closest: function(selectorList) {
            var selectors = String(selectorList).split(',').map(function(value) { return value.trim(); });
            for (var i = 0; i < selectors.length; i++) {
                if (selectors[i] === '.lxmf-msg' && this._messageBubble) return this._messageBubble;
                if (selectors[i].charAt(0) === '.' && own.has(selectors[i].slice(1))) return this;
            }
            return null;
        }
    };
}

// Exercise the production recognizer with the production exclusion/ownership
// helpers. This catches the issue #45 regression: ordinary text must reach the
// first-stage action sheet, but elevated message text must belong to native selection.
(function testLongPressOwnership() {
    var now = 10;
    var nextRaf = 1;
    var rafs = new Map();
    var nextTimer = 1;
    var timers = new Map();
    var haptics = [];
    var documentStub = new EventTargetStub();
    documentStub.hidden = false;
    documentStub.documentElement = { dataset: { inputModality: 'touch' } };
    var context = {
        window: { RS: { gestures: {} } },
        document: documentStub,
        performance: { now: function() { return now; } },
        requestAnimationFrame: function(callback) {
            var id = nextRaf++;
            rafs.set(id, callback);
            return id;
        },
        cancelAnimationFrame: function(id) { rafs.delete(id); },
        setTimeout: function(callback, delay) {
            var id = nextTimer++;
            timers.set(id, { callback: callback, due: now + delay });
            return id;
        },
        clearTimeout: function(id) { timers.delete(id); },
        haptic: function(level) { haptics.push(level); },
        isMobile: function() { return true; },
    };
    context.RS = context.window.RS;
    vm.runInNewContext(gesturesSource, context, { filename: 'gestures.js' });
    vm.runInNewContext(
        'var _activeContextMenu = null; var lxmfActiveContact = "owner-a"; var _pendingMessageHoldActivation = null; var _messageHoldActivationSequence = 0;\n' +
        'function _canonicalConversationHash(value) { return String(value || "").toLowerCase(); }\n' +
        namedFunctionSource(lxmfSource, '_messageActionOwnsText') + '\n' +
        namedFunctionSource(lxmfSource, '_messageElevatedTextStartsNativeSelection') + '\n' +
        namedFunctionSource(lxmfSource, '_messageTouchStartsDirectControl') + '\n' +
        namedFunctionSource(lxmfSource, '_messageHoldActivationSurface') + '\n' +
        namedFunctionSource(lxmfSource, '_clearPendingMessageHoldActivation') + '\n' +
        namedFunctionSource(lxmfSource, '_armPendingMessageHoldActivation') + '\n' +
        namedFunctionSource(lxmfSource, '_releasePendingMessageHoldActivation') + '\n' +
        namedFunctionSource(lxmfSource, '_pendingMessageHoldContextMatches') + '\n' +
        namedFunctionSource(lxmfSource, '_consumePendingMessageHoldContext') + '\n' +
        namedFunctionSource(lxmfSource, '_consumePendingMessageHoldActivation') + '\n' +
        'this.setActionOwner = function(bubble) { _activeContextMenu = bubble ? { bubble: bubble, ownerHash: "owner-a" } : null; };\n' +
        'this.startsNativeSelection = _messageElevatedTextStartsNativeSelection;\n' +
        'this.startsDirectControl = _messageTouchStartsDirectControl;\n' +
        'this.armActivation = _armPendingMessageHoldActivation;\n' +
        'this.releaseActivation = _releasePendingMessageHoldActivation;\n' +
        'this.cancelActivation = _clearPendingMessageHoldActivation;\n' +
        'this.consumeContext = _consumePendingMessageHoldContext;\n' +
        'this.consumeActivation = _consumePendingMessageHoldActivation;\n' +
        'this.hasPendingActivation = function() { return !!_pendingMessageHoldActivation; };',
        context,
        { filename: 'message-action-policy.js' }
    );

    function advance(milliseconds) {
        now += milliseconds;
        var pending = Array.from(rafs.values());
        rafs.clear();
        pending.forEach(function(callback) { callback(now); });
        Array.from(timers.entries()).forEach(function(entry) {
            if (entry[1].due > now) return;
            timers.delete(entry[0]);
            entry[1].callback();
        });
    }

    function beginGesture(target, elevated) {
        var element = new EventTargetStub();
        var bubble = {
            contains: function(candidate) { return candidate === target; },
            getAttribute: function(name) { return name === 'data-msg-id' ? 'message-1' : null; },
        };
        target._messageBubble = bubble;
        var state = { fired: 0 };
        context.setActionOwner(elevated ? bubble : null);
        context.RS.gestures.attachLongPress(element, {
            duration: 500,
            moveCancelPx: 12,
            excludeZone: function(touch) {
                return context.startsNativeSelection(touch, bubble) ||
                    context.startsDirectControl(touch, bubble);
            },
            hapticStages: [{ at: 0.55, level: 'light' }],
            onFire: function(touch) {
                state.fired += 1;
                context.armActivation(bubble, 'message-1', touch);
            },
        });
        element.addEventListener('touchend', function() { context.releaseActivation(bubble, now); });
        element.addEventListener('touchcancel', function() { context.cancelActivation(); });
        element.dispatch('touchstart', {
            touches: [{ target: target, clientX: 10, clientY: 10 }],
            cancelable: true,
            preventDefault: function() {},
        });
        return { element: element, target: target, bubble: bubble, state: state };
    }

    function hold(target, elevated) {
        var gesture = beginGesture(target, elevated);
        advance(501);
        return gesture.state.fired;
    }

    assert.strictEqual(hold(targetWithClasses(['lxmf-msg-content']), false), 1,
        'ordinary message text must open actions on its first touch hold');
    assert.strictEqual(hold(targetWithClasses([]), false), 1,
        'blank message padding must open the same actions as message text');
    assert.strictEqual(hold(targetWithClasses(['lxmf-msg-meta']), false), 1,
        'the blank/meta line beneath message text must open the same actions');
    assert.strictEqual(hold(targetWithClasses(['rs-message-link']), false), 1,
        'touch links must keep message actions reachable');
    assert.strictEqual(hold(targetWithClasses(['lxmf-clickable-img']), false), 1,
        'image surfaces must open the same message actions');
    var hapticsBeforeSelection = haptics.length;
    assert.strictEqual(hold(targetWithClasses(['lxmf-msg-content']), true), 0,
        'elevated message text must yield its next hold to native text selection');
    assert.strictEqual(haptics.length, hapticsBeforeSelection,
        'native selection ownership must not leak a staged reaction haptic');
    assert.strictEqual(hold(targetWithClasses(['voice-memo-player-play']), false), 0,
        'direct playback controls must own their touch');
    assert.strictEqual(hold(targetWithClasses(['msg-actions-trigger']), false), 0,
        'the explicit More control must own its touch without a duplicate hold action');

    function activationEvent() {
        return {
            cancelable: true,
            prevented: false,
            stopped: false,
            preventDefault: function() { this.prevented = true; },
            stopPropagation: function() { this.stopped = true; },
        };
    }

    [
        ['lxmf-clickable-img', 'image'],
        ['rs-message-link', 'link'],
        ['rs-file-download', 'file'],
        ['msg-reply-quote', 'reply quote'],
    ].forEach(function(surfaceCase) {
        var surface = targetWithClasses([surfaceCase[0]]);
        var gesture = beginGesture(surface, false);
        advance(501);
        gesture.element.dispatch('touchend', {});
        var event = activationEvent();
        assert.strictEqual(context.consumeActivation(event, surface, now), true,
            surfaceCase[1] + ' synthetic click must be consumed after its successful hold');
        assert.strictEqual(context.consumeActivation(activationEvent(), surface, now), false,
            surfaceCase[1] + ' guard must be exactly one-shot');
        assert.strictEqual(event.prevented, true);
        assert.strictEqual(event.stopped, true);
    });

    var originalSurface = targetWithClasses(['rs-message-link']);
    var originalGesture = beginGesture(originalSurface, false);
    advance(501);
    originalGesture.element.dispatch('touchend', {});
    var unrelatedSurface = targetWithClasses(['rs-message-link']);
    unrelatedSurface._messageBubble = { contains: function() { return true; } };
    assert.strictEqual(context.consumeActivation(activationEvent(), unrelatedSurface, now), false,
        'an unrelated message tap must never be swallowed');
    assert.strictEqual(context.consumeActivation(activationEvent(), originalSurface, now), true,
        'the exact held surface must remain armed after an unrelated tap');

    var firstLink = targetWithClasses(['rs-message-link']);
    var firstLinkGesture = beginGesture(firstLink, false);
    advance(501);
    firstLinkGesture.element.dispatch('touchend', {});
    var secondLink = targetWithClasses(['rs-message-link']);
    secondLink._messageBubble = firstLinkGesture.bubble;
    assert.strictEqual(context.consumeActivation(activationEvent(), secondLink, now), false,
        'a deliberate tap on another link in the same message must not be swallowed');
    assert.strictEqual(context.consumeActivation(activationEvent(), firstLink, now), true,
        'the synthetic activation remains bound to the exact held link');

    var longSurface = targetWithClasses(['rs-file-download']);
    var longGesture = beginGesture(longSurface, false);
    advance(501);
    advance(1200);
    longGesture.element.dispatch('touchend', {});
    assert.strictEqual(context.consumeActivation(activationEvent(), longSurface, now), true,
        'a hold longer than the fallback window must remain armed through release');

    var noClickSurface = targetWithClasses(['lxmf-clickable-img']);
    var noClickGesture = beginGesture(noClickSurface, false);
    advance(501);
    noClickGesture.element.dispatch('touchend', {});
    assert.strictEqual(context.hasPendingActivation(), true);
    advance(751);
    assert.strictEqual(context.hasPendingActivation(), false,
        'a hold with no synthesized click must expire after release');

    var contextFirstSurface = targetWithClasses(['rs-message-link']);
    var contextFirstGesture = beginGesture(contextFirstSurface, false);
    advance(501);
    assert.strictEqual(context.consumeContext(contextFirstSurface, contextFirstGesture.bubble, now), true);
    assert.strictEqual(context.consumeContext(contextFirstSurface, contextFirstGesture.bubble, now), true,
        'duplicate synthetic contextmenu must remain suppressed without reopening actions');
    contextFirstGesture.element.dispatch('touchend', {});
    assert.strictEqual(context.consumeActivation(activationEvent(), contextFirstSurface, now), true,
        'contextmenu before release must not disarm the synthesized click guard');

    var activationFirstSurface = targetWithClasses(['lxmf-clickable-img']);
    var activationFirstGesture = beginGesture(activationFirstSurface, false);
    advance(501);
    activationFirstGesture.element.dispatch('touchend', {});
    assert.strictEqual(context.consumeActivation(activationEvent(), activationFirstSurface, now), true);
    assert.strictEqual(context.consumeContext(activationFirstSurface, activationFirstGesture.bubble, now), true,
        'contextmenu arriving after the synthesized click must still be suppressed');

    var cancellationHaptics = haptics.length;
    var shortTap = beginGesture(targetWithClasses(['lxmf-msg-content']), false);
    shortTap.element.dispatch('touchend', {});
    advance(501);
    assert.strictEqual(shortTap.state.fired, 0, 'a short tap must not open message actions');

    var panned = beginGesture(targetWithClasses(['lxmf-msg-content']), false);
    panned.element.dispatch('touchmove', {
        touches: [{ target: panned.target, clientX: 23, clientY: 10 }],
    });
    advance(501);
    assert.strictEqual(panned.state.fired, 0, 'a pan beyond the 12px threshold must cancel actions');

    var cancelled = beginGesture(targetWithClasses(['lxmf-msg-content']), false);
    cancelled.element.dispatch('touchcancel', {});
    advance(501);
    assert.strictEqual(cancelled.state.fired, 0, 'touch cancellation must cancel actions');

    var hidden = beginGesture(targetWithClasses(['lxmf-msg-content']), false);
    documentStub.hidden = true;
    documentStub.dispatch('visibilitychange', {});
    advance(501);
    documentStub.hidden = false;
    assert.strictEqual(hidden.state.fired, 0, 'visibility loss must cancel actions');
    assert.strictEqual(haptics.length, cancellationHaptics,
        'cancelled recognizers must not emit staged haptics');
}());

// Copy Message uses canonical plaintext, not DOM textContent. That preserves
// line breaks around linkified anchors and strips only structured file fallback.
(function testCanonicalCopyText() {
    var context = {};
    vm.runInNewContext(
        namedFunctionSource(lxmfSource, '_messageDisplayContent') + '\n' +
        'this.displayContent = _messageDisplayContent;',
        context,
        { filename: 'message-display-content.js' }
    );
    var multiline = 'First line\nhttps://example.org/path\nLast line';
    assert.strictEqual(context.displayContent({ content: multiline }, false), multiline);
    assert.strictEqual(
        context.displayContent({ content: multiline + '\n[File: photo.jpg]', image: {} }, false),
        multiline
    );
    assert.strictEqual(context.displayContent({ content: 'Voice message', audio: {} }, true), '');
}());

function classListStub() {
    var names = new Set();
    return {
        add: function(name) { names.add(name); },
        remove: function(name) { names.delete(name); },
        contains: function(name) { return names.has(name); },
    };
}

// Assistive-technology clicks can arrive with touch as the last raw modality.
// Their explicit focus expectation must survive optimistic reaction rerenders.
(function testAssistiveReactionFocusRestoration() {
    var reactionPill = {
        getAttribute: function(name) {
            if (name === 'data-msg-id') return 'message-1';
            if (name === 'data-emoji') return '👍';
            return null;
        },
        focus: function() { this.focused = true; },
    };
    var moreTrigger = { focus: function() { this.focused = true; } };
    var bubble = {
        getAttribute: function(name) { return name === 'data-msg-id' ? 'message-1' : null; },
        querySelector: function(selector) { return selector === '.msg-actions-trigger' ? moreTrigger : null; },
    };
    var container = {
        querySelectorAll: function(selector) {
            if (selector === '.lxmf-msg[data-msg-id]') return [bubble];
            if (selector === '.reaction-pill[data-msg-id]') return [reactionPill];
            return [];
        },
    };
    var documentStub = {
        documentElement: { dataset: { inputModality: 'touch' } },
        getElementById: function(id) { return id === 'lxmf-messages' ? container : null; },
    };
    var context = {
        document: documentStub,
        window: { RS: { ui: { prefersKeyboardFocus: function() { return false; } } } },
    };
    context.RS = context.window.RS;
    vm.runInNewContext(
        namedFunctionSource(lxmfSource, '_prefersKeyboardMessageFocus') + '\n' +
        namedFunctionSource(lxmfSource, '_messageActivationExpectsFocus') + '\n' +
        namedFunctionSource(lxmfSource, '_focusMessageControl') + '\n' +
        namedFunctionSource(lxmfSource, '_restoreRenderedMessageActionFocus') + '\n' +
        'this.expectsFocus = _messageActivationExpectsFocus;\n' +
        'this.restoreReactionFocus = _restoreRenderedMessageActionFocus;',
        context,
        { filename: 'message-reaction-focus.js' }
    );
    var expectsFocus = context.expectsFocus({ type: 'click', detail: 0 });
    assert.strictEqual(expectsFocus, true);
    context.restoreReactionFocus('message-1', '👍', expectsFocus);
    assert.strictEqual(reactionPill.focused, true,
        'AT reaction activation must focus the fresh semantic reaction pill');
    reactionPill.focused = false;
    context.restoreReactionFocus('message-1', '❤️', expectsFocus);
    assert.strictEqual(moreTrigger.focused, true,
        'when the exact pill is absent after rollback, focus must return to fresh More');
}());

// Progress and reaction updates must not detach a pressed action button. The
// same action DOM remains live until activation, then one latest render flushes.
(function testActionRenderDeferral() {
    var context = {};
    vm.runInNewContext(
        'var _activeContextMenu = null;\n' +
        'var _deferredConversationRenderOptions = null; var _deferredConversationRenderOwnerHash = null;\n' +
        'var _deferredConversationRenderGeneration = 0; var _deferredConversationRenderRelease = null;\n' +
        'var lxmfConversation = [{ id: "message-1" }]; var lxmfActiveContact = "owner-a";\n' +
        'var renderCalls = []; var _activationCount = 0;\n' +
        'function _canonicalConversationHash(value) { return String(value || "").toLowerCase(); }\n' +
        'function _cancelScheduledDeferredConversationRender() { return false; }\n' +
        'function _pendingRenderReleaseOwnsCurrentConversation() { return false; }\n' +
        'function _findRenderedMessageBubble() { return null; }\n' +
        'function _focusMessageControl() {}\n' +
        'function renderConversation(options) { renderCalls.push(options); }\n' +
        namedFunctionSource(lxmfSource, '_mergeConversationRenderOptions') + '\n' +
        namedFunctionSource(lxmfSource, '_activeActionOwnsCurrentMessage') + '\n' +
        namedFunctionSource(lxmfSource, '_deferConversationRender') + '\n' +
        namedFunctionSource(lxmfSource, '_deferActiveMessageInteractionRender') + '\n' +
        namedFunctionSource(lxmfSource, '_clearDeferredConversationRender') + '\n' +
        namedFunctionSource(lxmfSource, '_flushDeferredConversationRender') + '\n' +
        namedFunctionSource(lxmfSource, '_dismissContextMenu') + '\n' +
        'this.open = function(button, msgId) {\n' +
        '  _activeContextMenu = {\n' +
        '    msgId: msgId || "message-1", ownerHash: "owner-a", trigger: null, restoreFocusExpected: false,\n' +
        '    menu: { button: button, parentNode: null }, row: null, container: null\n' +
        '  };\n' +
        '};\n' +
        'this.defer = _deferActiveMessageInteractionRender;\n' +
        'this.activate = function() { _activationCount += 1; _dismissContextMenu(); };\n' +
        'this.transfer = function(button, msgId) { _dismissContextMenu({ flushDeferredRender: false }); this.open(button, msgId); };\n' +
        'this.getButton = function() { return _activeContextMenu && _activeContextMenu.menu.button; };\n' +
        'this.renderCalls = renderCalls; this.activationCount = function() { return _activationCount; };\n' +
        'this.setOwner = function(owner) { lxmfActiveContact = owner; };\n' +
        'this.setMessages = function(messages) { lxmfConversation = messages; };',
        context,
        { filename: 'message-action-render-deferral.js' }
    );

    var actionButton = { id: 'same-live-action-node' };
    context.open(actionButton);
    assert.strictEqual(context.defer({ stickToBottom: true }), true);
    assert.strictEqual(context.defer({ forceScrollBottom: true }), true);
    assert.strictEqual(context.defer({}), true);
    assert.strictEqual(context.renderCalls.length, 0);
    assert.strictEqual(context.getButton(), actionButton,
        'progress-like rerenders must not replace an action button before activation');
    context.activate();
    assert.strictEqual(context.activationCount(), 1);
    assert.strictEqual(context.renderCalls.length, 1,
        'an activated menu action must flush all deferred updates once');
    assert.strictEqual(context.renderCalls[0].stickToBottom, true);
    assert.strictEqual(context.renderCalls[0].forceScrollBottom, true);

    var secondButton = { id: 'keyboard-or-at-more-b' };
    context.open(actionButton);
    context.defer({ stickToBottom: true });
    context.transfer(secondButton);
    assert.strictEqual(context.getButton(), secondButton,
        'keyboard/AT More-B activation must transfer the lease without replacing its target');
    assert.strictEqual(context.renderCalls.length, 1);
    context.activate();
    assert.strictEqual(context.renderCalls.length, 2,
        'the transferred action lease must still consolidate its queued render once');

    context.open(actionButton);
    context.setOwner('owner-b');
    assert.strictEqual(context.defer({}), false,
        'owner changes must force immediate normal render/teardown');
    context.setOwner('owner-a');
    context.setMessages([]);
    assert.strictEqual(context.defer({}), false,
        'message removal must force immediate normal render/teardown');
}());

// Outside pointer dismissal releases the visible UI immediately but retains a
// generation-bound transcript lease until that pointer's release/click task.
(function testOutsidePointerReleaseLease() {
    var nextTimer = 1;
    var timers = new Map();
    var documentStub = new EventTargetStub();
    var context = {
        document: documentStub,
        setTimeout: function(callback, delay) {
            var id = nextTimer++;
            timers.set(id, { callback: callback, delay: delay || 0 });
            return id;
        },
        clearTimeout: function(id) { timers.delete(id); },
    };
    vm.runInNewContext(
        'var _activeContextMenu = null;\n' +
        'var _deferredConversationRenderOptions = null; var _deferredConversationRenderOwnerHash = null;\n' +
        'var _deferredConversationRenderGeneration = 0; var _deferredConversationRenderRelease = null;\n' +
        'var lxmfActiveContact = "owner-a"; var lxmfConversation = [{ id: "message-1" }, { id: "message-2" }];\n' +
        'var renderLog = [];\n' +
        'function _canonicalConversationHash(value) { return String(value || "").toLowerCase(); }\n' +
        'function _findRenderedMessageBubble() { return null; }\n' +
        'function _focusMessageControl() {}\n' +
        namedFunctionSource(lxmfSource, '_messageActionOwnsText') + '\n' +
        namedFunctionSource(lxmfSource, '_mergeConversationRenderOptions') + '\n' +
        namedFunctionSource(lxmfSource, '_activeActionOwnsCurrentMessage') + '\n' +
        namedFunctionSource(lxmfSource, '_deferConversationRender') + '\n' +
        namedFunctionSource(lxmfSource, '_pendingRenderReleaseOwnsCurrentConversation') + '\n' +
        namedFunctionSource(lxmfSource, '_deferActiveMessageInteractionRender') + '\n' +
        namedFunctionSource(lxmfSource, '_clearDeferredConversationRender') + '\n' +
        namedFunctionSource(lxmfSource, '_takeDeferredConversationRenderOptions') + '\n' +
        namedFunctionSource(lxmfSource, '_cancelScheduledDeferredConversationRender') + '\n' +
        namedFunctionSource(lxmfSource, '_scheduleDeferredConversationRenderAfterPointer') + '\n' +
        namedFunctionSource(lxmfSource, '_flushDeferredConversationRender') + '\n' +
        namedFunctionSource(lxmfSource, '_dismissContextMenu') + '\n' +
        namedFunctionSource(lxmfSource, '_handleMessageActionPointer') + '\n' +
        'function _requestRender(options) {\n' +
        '  if (_deferActiveMessageInteractionRender(options)) return "deferred";\n' +
        '  renderLog.push({ owner: _canonicalConversationHash(lxmfActiveContact), options: _takeDeferredConversationRenderOptions(options) });\n' +
        '  return "rendered";\n' +
        '}\n' +
        'function renderConversation(options) { return _requestRender(options); }\n' +
        'this.open = function(msgId, button) {\n' +
        '  _activeContextMenu = {\n' +
        '    msgId: msgId, ownerHash: _canonicalConversationHash(lxmfActiveContact), trigger: null, restoreFocusExpected: false,\n' +
        '    menu: { parentNode: null, button: button, contains: function(target) { return target === button; } },\n' +
        '    row: null, container: null\n' +
        '  };\n' +
        '};\n' +
        'this.outside = _handleMessageActionPointer; this.requestRender = _requestRender;\n' +
        'this.dismiss = _dismissContextMenu; this.renderLog = renderLog;\n' +
        'this.hasActive = function() { return !!_activeContextMenu; };\n' +
        'this.hasDeferred = function() { return !!_deferredConversationRenderOptions; };\n' +
        'this.setOwner = function(owner) { lxmfActiveContact = owner; };',
        context,
        { filename: 'message-outside-pointer-lease.js' }
    );

    function runTimers(maxDelay) {
        var pending = Array.from(timers.entries()).filter(function(entry) {
            return typeof maxDelay !== 'number' || entry[1].delay <= maxDelay;
        });
        pending.forEach(function(entry) {
            timers.delete(entry[0]);
            entry[1].callback();
        });
    }
    function resetLog() { context.renderLog.splice(0, context.renderLog.length); }

    var originalButton = { id: 'menu-a' };
    var outsideLink = { id: 'outside-link' };
    context.open('message-1', originalButton);
    context.outside({ target: outsideLink });
    assert.strictEqual(context.hasActive(), false);
    assert.strictEqual(context.requestRender({ forceScrollBottom: true }), 'deferred',
        'the first network progress after pointerdown must remain under an empty release lease');
    documentStub.dispatch('pointerup', {});
    var outsideActivations = 0;
    outsideActivations += 1; // target click dispatch runs before the queued task
    documentStub.dispatch('click', { target: outsideLink });
    assert.strictEqual(outsideActivations, 1);
    assert.strictEqual(context.renderLog.length, 0,
        'outside target activation must run before transcript replacement');
    runTimers();
    assert.strictEqual(context.renderLog.length, 1);
    assert.strictEqual(context.renderLog[0].options.stickToBottom, false);
    assert.strictEqual(context.renderLog[0].options.forceScrollBottom, true);

    resetLog();
    context.open('message-1', originalButton);
    context.requestRender({ stickToBottom: true });
    context.outside({ target: outsideLink });
    documentStub.dispatch('pointerup', {});
    var nextButton = { id: 'menu-b' };
    context.open('message-2', nextButton); // the outside click opened another More
    documentStub.dispatch('click', { target: nextButton });
    runTimers();
    assert.strictEqual(context.renderLog.length, 0,
        'a new action lease must keep the scheduled flush deferred on stable DOM');
    assert.strictEqual(context.hasDeferred(), true);
    context.dismiss();
    assert.strictEqual(context.renderLog.length, 1,
        'the transferred lease must release one consolidated render on final dismiss');

    resetLog();
    context.open('message-1', originalButton);
    context.requestRender({ stickToBottom: true });
    context.outside({ target: outsideLink });
    documentStub.dispatch('pointerup', {});
    context.setOwner('owner-b');
    assert.strictEqual(context.requestRender({ forceScrollBottom: true }), 'rendered');
    assert.strictEqual(context.renderLog[0].owner, 'owner-b');
    assert.strictEqual(context.renderLog[0].options.stickToBottom, false,
        'owner B must never merge owner A scroll intent');
    context.open('message-1', originalButton);
    assert.strictEqual(context.requestRender({ stickToBottom: true }), 'deferred');
    runTimers();
    assert.strictEqual(context.hasDeferred(), true,
        'a stale owner-A release task must not clear the newer owner-B queue generation');
    context.dismiss();
    assert.strictEqual(context.renderLog.length, 2);
    assert.strictEqual(context.renderLog[1].owner, 'owner-b');

    resetLog();
    context.setOwner('owner-a');
    context.open('message-1', originalButton);
    context.requestRender({ stickToBottom: true });
    context.outside({ target: outsideLink });
    documentStub.dispatch('touchcancel', {});
    runTimers(0);
    assert.strictEqual(context.renderLog.length, 1,
        'touch cancellation must release the pending lease on the next task');

    resetLog();
    context.open('message-1', originalButton);
    context.requestRender({ stickToBottom: true });
    context.outside({ target: outsideLink });
    documentStub.dispatch('touchend', {});
    assert.strictEqual(context.renderLog.length, 0);
    // A platform-delayed synthetic click still runs before the 450ms fallback.
    documentStub.dispatch('click', { target: outsideLink });
    runTimers(0);
    assert.strictEqual(context.renderLog.length, 1,
        'a delayed synthetic click must activate before the coalesced render');
}());

// Link context is deliberately modality-specific: native on pointer desktops,
// message actions on the first touch hold, then native selection while elevated.
(function testLinkContextOwnership() {
    var documentStub = { documentElement: { dataset: { inputModality: 'pointer' } } };
    var selection = { isCollapsed: true, rangeCount: 0 };
    var context = {
        document: documentStub,
        window: { getSelection: function() { return selection; } },
    };
    vm.runInNewContext(
        'var _activeContextMenu = null; var _messagePointerContextSelection = null; var lxmfActiveContact = "owner-a"; var pendingContext = false;\n' +
        'function _canonicalConversationHash(value) { return String(value || "").toLowerCase(); }\n' +
        'function _consumePendingMessageHoldContext() { return pendingContext; }\n' +
        namedFunctionSource(lxmfSource, '_messageActionOwnsText') + '\n' +
        namedFunctionSource(lxmfSource, '_messageSelectionIntersectsBubble') + '\n' +
        namedFunctionSource(lxmfSource, '_messageLinkUsesNativeContext') + '\n' +
        namedFunctionSource(lxmfSource, '_messageLinkActivationAllowed') + '\n' +
        namedFunctionSource(lxmfSource, '_rememberMessagePointerContextSelection') + '\n' +
        namedFunctionSource(lxmfSource, '_consumeMessagePointerContextSelection') + '\n' +
        namedFunctionSource(lxmfSource, '_messageContextMenuDisposition') + '\n' +
        'this.setOwner = function(bubble) { _activeContextMenu = bubble ? { bubble: bubble, ownerHash: "owner-a" } : null; };\n' +
        'this.setPendingContext = function(value) { pendingContext = value; };\n' +
        'this.rememberPointerSelection = _rememberMessagePointerContextSelection;\n' +
        'this.consumePointerSelection = _consumeMessagePointerContextSelection;\n' +
        'this.nativeLink = _messageLinkUsesNativeContext;\n' +
        'this.linkAllowed = _messageLinkActivationAllowed;\n' +
        'this.disposition = _messageContextMenuDisposition;',
        context,
        { filename: 'message-link-context.js' }
    );
    var bubble = { contains: function(candidate) { return candidate === link || candidate === text; } };
    var text;
    var link = {
        closest: function(selector) {
            if (selector === '.rs-message-link' || selector === '.lxmf-msg-content') return link;
            if (selector === '.lxmf-msg') return bubble;
            return null;
        }
    };
    assert.strictEqual(context.nativeLink(link), true);
    assert.strictEqual(context.disposition(link, bubble, 10), 'native');
    text = {
        closest: function(selector) {
            if (selector === '.lxmf-msg-content') return text;
            if (selector === '.lxmf-msg') return bubble;
            return null;
        }
    };
    assert.strictEqual(context.rememberPointerSelection({ button: 2 }, bubble), true);
    selection = {
        isCollapsed: false,
        rangeCount: 1,
        getRangeAt: function() { return { intersectsNode: function() { return true; } }; },
    };
    var selectionBeforeRightClick = context.consumePointerSelection(bubble);
    assert.strictEqual(selectionBeforeRightClick, false,
        'the pointer snapshot must precede the browser selecting a right-clicked word');
    assert.strictEqual(context.disposition(text, bubble, 10, selectionBeforeRightClick), 'actions',
        'a right-click-created word highlight must open message actions, not the native copy menu');
    assert.strictEqual(context.disposition(text, bubble, 10, true), 'native',
        'a pre-existing left-drag selection must retain the native copy menu');
    selection = { isCollapsed: true, rangeCount: 0 };
    documentStub.documentElement.dataset.inputModality = 'touch';
    assert.strictEqual(context.nativeLink(link), false);
    assert.strictEqual(context.linkAllowed(link), true);
    assert.strictEqual(context.disposition(link, bubble, 10), 'actions');
    context.setOwner(bubble);
    assert.strictEqual(context.linkAllowed(link), false,
        'an elevated message must suppress link navigation during native selection');
    context.setPendingContext(true);
    assert.strictEqual(context.disposition(link, bubble, 100), 'suppress',
        'the first hold contextmenu must remain part of the action-opening gesture');
    context.setPendingContext(false);
    assert.strictEqual(context.disposition(link, bubble, 101), 'native',
        'elevated message text must yield its contextmenu to native selection');
}());

// Pointer-down capture must not dismiss the elevated action surface before a
// second mobile hold can establish native selection on the same text node.
(function testElevatedTextKeepsActionLease() {
    var context = {};
    vm.runInNewContext(
        'var _activeContextMenu = null; var lxmfActiveContact = "owner-a"; var _dismissCalls = 0;\n' +
        'function _canonicalConversationHash(value) { return String(value || "").toLowerCase(); }\n' +
        'function _dismissContextMenu() { _dismissCalls += 1; _activeContextMenu = null; return true; }\n' +
        'function _scheduleDeferredConversationRenderAfterPointer() {}\n' +
        namedFunctionSource(lxmfSource, '_messageActionOwnsText') + '\n' +
        namedFunctionSource(lxmfSource, '_handleMessageActionPointer') + '\n' +
        'this.open = function(bubble) { _activeContextMenu = { bubble: bubble, ownerHash: "owner-a", menu: { contains: function() { return false; } } }; };\n' +
        'this.handle = _handleMessageActionPointer; this.getDismissCount = function() { return _dismissCalls; };\n' +
        'this.hasActive = function() { return !!_activeContextMenu; };',
        context,
        { filename: 'message-elevated-selection-lease.js' }
    );
    var text = {
        closest: function(selector) { return selector === '.lxmf-msg-content' ? text : null; },
    };
    var bubble = { contains: function(candidate) { return candidate === text; } };
    context.open(bubble);
    context.handle({ target: text });
    assert.strictEqual(context.hasActive(), true,
        'second-hold pointerdown on elevated text must keep the stable transcript DOM');
    assert.strictEqual(context.getDismissCount(), 0);
    context.handle({ target: {} });
    assert.strictEqual(context.hasActive(), false,
        'an outside pointerdown must still dismiss the action/selection surface');
    assert.strictEqual(context.getDismissCount(), 1);
}());

// Native Copy is the terminal action for an elevated mobile selection. Cleanup
// runs after the copy event so the platform can first serialize the range.
(function testNativeCopyDismissesElevatedMessage() {
    var pendingTasks = [];
    var selection = {
        isCollapsed: false,
        rangeCount: 1,
        getRangeAt: function() { return { intersectsNode: function() { return true; } }; },
        removeAllRanges: function() { this.cleared = true; },
    };
    var context = {
        window: { getSelection: function() { return selection; } },
        setTimeout: function(callback) { pendingTasks.push(callback); return pendingTasks.length; },
        isTauriMobile: function() { return true; },
        isMobile: function() { return true; },
    };
    vm.runInNewContext(
        'var _activeContextMenu = null; var _dismissCalls = 0;\n' +
        namedFunctionSource(lxmfSource, '_mobileMessageActionsUseLongPress') + '\n' +
        namedFunctionSource(lxmfSource, '_messageSelectionIntersectsBubble') + '\n' +
        namedFunctionSource(lxmfSource, '_clearNativeMessageSelection') + '\n' +
        'function _dismissContextMenu() { _dismissCalls += 1; _activeContextMenu = null; return true; }\n' +
        namedFunctionSource(lxmfSource, '_handleNativeMessageCopy') + '\n' +
        'this.open = function(value) { _activeContextMenu = value; }; this.copy = _handleNativeMessageCopy;\n' +
        'this.active = function() { return _activeContextMenu; }; this.getDismissCount = function() { return _dismissCalls; };',
        context,
        { filename: 'message-native-copy-cleanup.js' }
    );
    var active = { bubble: {} };
    context.open(active);
    assert.strictEqual(context.copy(), true);
    assert.strictEqual(context.active(), active,
        'copy handling must not replace the selected DOM before clipboard serialization');
    pendingTasks.shift()();
    assert.strictEqual(selection.cleared, true);
    assert.strictEqual(context.active(), null);
    assert.strictEqual(context.getDismissCount(), 1,
        'copy completion must return the mobile message to its base state');

    selection.cleared = false;
    var oldActive = { bubble: {} };
    var newActive = { bubble: {} };
    context.open(oldActive);
    assert.strictEqual(context.copy(), true);
    context.open(newActive);
    pendingTasks.shift()();
    assert.strictEqual(context.active(), newActive,
        'a delayed copy cleanup must never dismiss a newly opened message');
    assert.strictEqual(selection.cleared, false);
}());

assert(/\.lxmf-msg\s*\{[\s\S]*?user-select:\s*none;[\s\S]*?-webkit-user-select:\s*none;/.test(messagingCss),
    'general message chrome must remain non-selectable');
assert(messagingCss.includes('html[data-input-modality="touch"] .lxmf-msg-content'),
    'idle touch message text must not race the long-press action stage');
assert(messagingCss.includes('.lxmf-messages.msg-action-mode .msg-row.msg-action-selected .lxmf-msg-content'),
    'only the elevated message may opt into touch selection');
assert(messagingCss.includes('@media (hover: none) and (pointer: coarse)') &&
       messagingCss.includes('.lxmf-messages.msg-action-mode .msg-row.msg-action-selected .lxmf-msg-content'),
    'coarse mobile input must stage selection before the first touch event');
assert(messagingCss.includes('.msg-actions-trigger::before') && messagingCss.includes('var(--touch-target)'),
    'the restrained More control must retain a full touch target');
assert(/\.msg-actions-trigger\s*\{[\s\S]*?width:\s*0;[\s\S]*?opacity:\s*0;[\s\S]*?pointer-events:\s*none;/.test(messagingCss) &&
       /\.lxmf-msg:hover \.msg-actions-trigger,[\s\S]*?width:\s*24px;[\s\S]*?opacity:\s*1;/.test(messagingCss),
    'the compact plus control must appear only for its hovered/focused/expanded message');
assert(/html\[data-input-modality="touch"\] \.msg-actions-trigger\s*\{[\s\S]*?width:\s*0;[\s\S]*?opacity:\s*0;[\s\S]*?pointer-events:\s*none;/.test(messagingCss),
    'mobile touch UI must keep the redundant plus control visually hidden');
assert(lxmfSource.includes("mobileMessageActions ? ' msg-actions-trigger-mobile-hidden' : ''") &&
       /\.msg-actions-trigger\.msg-actions-trigger-mobile-hidden\s*\{[\s\S]*?position:\s*absolute;[\s\S]*?clip-path:\s*inset\(50%\);[\s\S]*?pointer-events:\s*none;/.test(messagingCss),
    'native and responsive mobile rendering must hide the plus structurally from visual layout and taps');
assert(lxmfSource.includes('<path d="M12 5v14M5 12h14"/>'),
    'message actions must use the compact plus glyph instead of an ellipsis');
var metaSource = lxmfSource.slice(
    lxmfSource.indexOf('var metaHtml = _messageProgressMetaHtml(msg)'),
    lxmfSource.indexOf('var bubbleClass = bubbleClassBase')
);
assert(metaSource.indexOf('msg-actions-trigger') < metaSource.indexOf('msg-time'),
    'the plus action must render to the left of the timestamp');
assert(messagingCss.includes('.msg-context-actions > :last-child:nth-child(odd)') &&
       messagingCss.includes('grid-column: 1 / -1;'),
    'an odd final action must span the dialog instead of leaving a visual hole');
assert(!messagingCss.includes('.msg-text-selection-guide') &&
       !lxmfSource.includes('Text selected. Adjust the selection or choose Done.') &&
       !lxmfSource.includes('<span>Select Text</span>') &&
       !lxmfSource.includes("data-message-action', 'select-text"),
    'selection must rely on the elevated bubble without a Select Text command or Done chrome');
assert(lxmfSource.includes("menu.setAttribute('role', 'dialog')") &&
       lxmfSource.includes("menu.setAttribute('aria-label', 'Message actions')"),
    'message actions must expose a labelled nonmodal dialog');
assert(!lxmfSource.includes("menu.setAttribute('role', 'menu')"),
    'message actions must not expose incomplete menu semantics');
assert(lxmfSource.includes('<button type="button" class="reaction-pill') &&
       lxmfSource.includes("'aria-pressed=\"' + (isMine ? 'true' : 'false')"),
    'persisted reactions must render as semantic pressed buttons');
assert(lxmfSource.includes('aria-haspopup="dialog" aria-expanded="false"') &&
       lxmfSource.includes("trigger.setAttribute('aria-expanded', 'true')") &&
       lxmfSource.includes("trigger.setAttribute('aria-expanded', 'false')"),
    'More must expose and maintain its dialog expansion state');

console.log('Message action and elevated native-selection behavior tests passed.');
