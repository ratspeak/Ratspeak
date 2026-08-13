#!/usr/bin/env node
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var root = path.join(__dirname, '..', '..');
var source = fs.readFileSync(path.join(root, 'dashboard/static/js/lxmf.js'), 'utf8');

function functionSource(name) {
    var start = source.indexOf('function ' + name + '(');
    assert.notStrictEqual(start, -1, name + ' must exist');
    var brace = source.indexOf('{', start);
    var depth = 0;
    var quote = '';
    var escaped = false;
    for (var i = brace; i < source.length; i++) {
        var ch = source[i];
        if (quote) {
            if (escaped) escaped = false;
            else if (ch === '\\') escaped = true;
            else if (ch === quote) quote = '';
            continue;
        }
        if (ch === '"' || ch === "'" || ch === '`') { quote = ch; continue; }
        if (ch === '{') depth++;
        if (ch === '}' && --depth === 0) return source.slice(start, i + 1);
    }
    throw new Error('unterminated function ' + name);
}

function deferred() {
    var resolve;
    var reject;
    var promise = new Promise(function(onResolve, onReject) {
        resolve = onResolve;
        reject = onReject;
    });
    return { promise: promise, resolve: resolve, reject: reject };
}

var stage = null;
var admission = null;
var calls = [];
var appended = [];
var context = {
    window: null,
    document: {
        getElementById: function() {
            return { value: '', style: {}, scrollTop: 0, blur: function() {} };
        },
        activeElement: null,
    },
    Promise: Promise,
    Blob: Blob,
    Uint8Array: Uint8Array,
    Error: Error,
    String: String,
    Date: Date,
    atob: atob,
    lxmfActiveContact: 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
    _conversationEpoch: 0,
    _conversationIdentityGeneration: 0,
    _notifyConversationOwnerChanged: function() {},
    generateMsgId: function() { return 'client-message'; },
    _deliveryPrefOrAuto: function() { return 'auto'; },
    _optimisticDeliveryMethod: function(value) { return value; },
    _stageAttachmentBlob: function() { return stage.promise; },
    _appendConversationMessage: function(hash, message) {
        appended.push({ hash: hash, message: message });
        return false;
    },
    _updateConversationPreview: function() {},
    loadConversations: function() {},
    _finishLxmfComposerSend: function() {},
    renderConversation: function() {},
    RS: {
        voiceMemos: { registerDraft: function() {} },
        invoke: function(command, payload) {
            calls.push({ command: command, payload: payload });
            if (command === 'cancel_attachment_stage') return Promise.resolve({ ok: true });
            if (command === 'send_lxmf_with_staged_attachment') return admission.promise;
            return Promise.reject(new Error('Unexpected command: ' + command));
        },
    },
};
context.window = context;
vm.createContext(context);
[
    '_canonicalConversationHash',
    '_conversationOwnerSnapshot',
    '_conversationOwnerIsCurrent',
    '_conversationOwnerIdentityIsCurrent',
    '_activateConversation',
    '_resetConversationSession',
    '_staleConversationOperationError',
    '_cancelStagedAttachmentToken',
    'sendLxmfVoiceMemo',
].forEach(function(name) {
    vm.runInContext(functionSource(name), context, { filename: 'lxmf-' + name + '.js' });
});

function voiceDraft() {
    return {
        data_base64: 'TFhWTQ==',
        duration_ms: 1200,
        waveform: [1, 2, 3],
        size: 4,
    };
}

async function flush() {
    for (var i = 0; i < 20; i++) await Promise.resolve();
}

(async function() {
    var hashA = context.lxmfActiveContact;
    var ownerA = context._conversationOwnerSnapshot();
    stage = deferred();
    admission = deferred();
    var staleSend = context.sendLxmfVoiceMemo(voiceDraft(), hashA, { owner: ownerA });
    context._activateConversation('bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', 'navigation');
    stage.resolve('stage-a');
    await assert.rejects(staleSend, /not sent after changing conversations/);
    assert(calls.some(function(call) {
        return call.command === 'cancel_attachment_stage' && call.payload.token === 'stage-a';
    }), 'navigation before admission must cancel the exact staged payload');
    assert(!calls.some(function(call) { return call.command === 'send_lxmf_with_staged_attachment'; }),
        'stale staging must never reach native send admission');

    calls.length = 0;
    context._activateConversation(hashA, 'navigation');
    var cancelledByRecorder = false;
    var cancelledOwner = context._conversationOwnerSnapshot();
    stage = deferred();
    admission = deferred();
    var cancelledSend = context.sendLxmfVoiceMemo(voiceDraft(), hashA, {
        owner: cancelledOwner,
        isCurrent: function() { return !cancelledByRecorder; },
    });
    cancelledByRecorder = true;
    stage.resolve('stage-cancelled');
    await assert.rejects(cancelledSend, /not sent after changing conversations/);
    assert(calls.some(function(call) {
        return call.command === 'cancel_attachment_stage' && call.payload.token === 'stage-cancelled';
    }), 'background or explicit retirement before admission must cancel staging even in the same chat');
    assert(!calls.some(function(call) { return call.command === 'send_lxmf_with_staged_attachment'; }));

    calls.length = 0;
    var admittedOwner = context._conversationOwnerSnapshot();
    var admissionStarted = false;
    stage = deferred();
    admission = deferred();
    var admittedSend = context.sendLxmfVoiceMemo(voiceDraft(), hashA, {
        owner: admittedOwner,
        onAdmissionStart: function() { admissionStarted = true; },
    });
    stage.resolve('stage-b');
    await flush();
    assert(admissionStarted, 'the UI must learn the exact native-admission boundary');
    context._activateConversation('bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', 'navigation');
    admission.resolve({ msg_id: 'accepted-b' });
    await admittedSend;
    assert(appended.some(function(entry) { return entry.hash === hashA && entry.message.id === 'accepted-b'; }),
        'same-identity navigation may reconcile the accepted message only into its original chat');

    var appendedBeforeIdentityReset = appended.length;
    context._activateConversation(hashA, 'navigation');
    var oldIdentityOwner = context._conversationOwnerSnapshot();
    stage = deferred();
    admission = deferred();
    var identityStaleSend = context.sendLxmfVoiceMemo(voiceDraft(), hashA, { owner: oldIdentityOwner });
    stage.resolve('stage-c');
    await flush();
    context._resetConversationSession('identity_replaced');
    admission.resolve({ msg_id: 'accepted-c' });
    await identityStaleSend;
    assert.strictEqual(appended.length, appendedBeforeIdentityReset,
        'an old-identity completion must not repopulate the replacement identity cache');

    var attachmentBranch = source.slice(
        source.indexOf('if (lxmfPendingFile)'),
        source.indexOf('if (!text) return;', source.indexOf('if (lxmfPendingFile)'))
    );
    assert(attachmentBranch.includes('_conversationOwnerIsCurrent(sendOwner)'));
    assert(attachmentBranch.includes('_cancelStagedAttachmentToken(stageToken)'));
    assert(attachmentBranch.includes('_conversationOwnerIdentityIsCurrent(sendOwner)'));

    console.log('Voice send ownership tests passed');
})().catch(function(error) {
    console.error(error);
    process.exitCode = 1;
});
