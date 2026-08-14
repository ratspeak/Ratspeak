'use strict';

const assert = require('assert');
const fs = require('fs');
const path = require('path');
const vm = require('vm');

const source = fs.readFileSync(path.join(__dirname, '..', 'static', 'js', 'lxmf.js'), 'utf8');

function between(start, end) {
    const from = source.indexOf(start);
    const to = source.indexOf(end, from + start.length);
    assert(from >= 0 && to > from, `missing source section ${start}`);
    return source.slice(from, to);
}

const calls = [];
const context = {
    Promise,
    lxstVoiceState: { incoming: null },
    _voiceAnswerToken: 0,
    _voiceCancelMemoForCall: () => Promise.resolve(),
    _voiceEnsurePlaybackReady: () => Promise.resolve(),
    _voiceEnsureMicrophonePermission: () => Promise.resolve(),
    _voiceStopRingtone: () => {},
    _voiceHaptic: () => {},
    _voiceResetCallControls: () => {},
    _voicePrimeNativeCallRoute: () => {},
    _voiceReleaseNativeCallRoutePrime: () => {},
    _voiceNotify: () => {},
    renderVoiceUi: () => {},
    RS: {
        invoke: (name, payload) => {
            calls.push({ name, payload });
            return Promise.resolve({ status: 'connecting' });
        }
    }
};
vm.createContext(context);
vm.runInContext(
    between('function _voiceIncomingIsExact(', 'function _voiceSetOptimisticOutgoing(') +
    between('function _voiceAnswerCall()', 'function _voiceRejectCall()'),
    context
);

(async function run() {
    context.lxstVoiceState.incoming = {
        link_id: '11'.repeat(16),
        remote_identity: '22'.repeat(16),
        status: 'ringing'
    };
    await context._voiceAnswerCall();
    assert.strictEqual(calls.length, 1);
    assert.strictEqual(calls[0].name, 'voice_answer');
    assert.strictEqual(calls[0].payload.args.link_id, '11'.repeat(16));
    assert.strictEqual(context.lxstVoiceState.incoming.link_id, '11'.repeat(16));
    assert.strictEqual(context.lxstVoiceState.incoming.status, 'connecting');

    let releasePermission;
    context._voiceEnsureMicrophonePermission = () => new Promise(resolve => {
        releasePermission = resolve;
    });
    context.lxstVoiceState.incoming = {
        link_id: '33'.repeat(16),
        remote_identity: '44'.repeat(16),
        status: 'ringing'
    };
    const staleAnswer = context._voiceAnswerCall();
    await new Promise(resolve => setImmediate(resolve));
    assert.strictEqual(typeof releasePermission, 'function');
    context.lxstVoiceState.incoming = {
        link_id: '55'.repeat(16),
        remote_identity: '66'.repeat(16),
        status: 'ringing'
    };
    releasePermission();
    await staleAnswer;
    assert.strictEqual(calls.length, 1, 'a replacement call must fence the stale answer');
    assert.strictEqual(context.lxstVoiceState.incoming.link_id, '55'.repeat(16));

    context._voiceEnsureMicrophonePermission = () => Promise.resolve();
    context.RS.invoke = () => Promise.reject(new Error('answer rejected'));
    await context._voiceAnswerCall();
    assert.strictEqual(context.lxstVoiceState.incoming.status, 'ringing');

    console.log('Voice call ownership tests passed');
})().catch(error => {
    console.error(error);
    process.exit(1);
});
