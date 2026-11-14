#!/usr/bin/env node
// A key, phrase or access file is released with its identity's PIN, or after
// a confirmation when it has none. A wrong PIN stays in the PIN sheet; a
// backup takes the identity's PIN beside its own.
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const js = path.join(__dirname, '..', '..', 'dashboard', 'static', 'js');
const identitySource = fs.readFileSync(path.join(js, 'identity.js'), 'utf8');
const stateSource = fs.readFileSync(path.join(js, 'state.js'), 'utf8');

function functionSource(source, name) {
    const start = source.indexOf('function ' + name + '(');
    assert.notEqual(start, -1, name + ' must exist');
    let depth = 0;
    for (let index = source.indexOf('{', start); index < source.length; index++) {
        if (source[index] === '{') depth++;
        if (source[index] === '}' && --depth === 0) return source.slice(start, index + 1);
    }
    throw new Error('unterminated ' + name);
}

const HASH = 'ab'.repeat(16);

function load({ confirm = true, protectedIdentity = false, format = 'reticulum', replies = {} } = {}) {
    const seen = { confirms: [], modals: [], closed: 0, invoked: [], shown: [], saved: [], toasts: [] };
    const fields = new Map();
    const field = id => {
        if (!fields.has(id)) fields.set(id, { value: '', textContent: '', style: {}, dataset: {} });
        return fields.get(id);
    };
    const target = { hash: HASH, nickname: 'Fir', passcode_protected: protectedIdentity };
    const context = {
        console,
        atob,
        RECOVERY_PHRASE_WORDS: 12,
        document: { getElementById: field },
        rsConfirm(opts) {
            seen.confirms.push(opts);
            return Promise.resolve(confirm);
        },
        showIdentityModal(title, body, onConfirm, confirmClass) {
            seen.modals.push({ title, body, onConfirm, confirmClass });
        },
        closeIdentityModal() { seen.closed++; },
        showRecoveryPhraseBackup(mnemonic) { seen.shown.push(mnemonic); },
        saveIdentityExportPayload(payload) {
            seen.saved.push(payload.label);
            return Promise.resolve();
        },
        handleIdentityExportError(error) { throw error; },
        showToast(...args) { seen.toasts.push(args); },
        showPreConditionToast(message) { seen.toasts.push([message]); },
        chooseIdentityExportFormat: () => Promise.resolve(format),
        identityByHash: hash => (hash === HASH ? target : null),
        identityDisplayName: ident => ident.nickname,
        window: {},
        RS: {
            invoke(name, args) {
                seen.invoked.push([name, JSON.parse(JSON.stringify(args))]);
                const reply = (replies[name] || []).shift();
                if (reply instanceof Error) return Promise.reject(reply);
                return Promise.resolve(reply || { mnemonic: 'words', data_base64: 'AA==', backup_base64: 'AA==' });
            }
        }
    };
    vm.runInNewContext([
        functionSource(stateSource, 'escapeHtml'),
        functionSource(identitySource, 'identityPinError'),
        functionSource(identitySource, 'identityField'),
        functionSource(identitySource, 'identityErrorHtml'),
        functionSource(identitySource, 'base64ToBytes'),
        functionSource(identitySource, 'backupPinFieldsHtml'),
        functionSource(identitySource, 'readBackupPin'),
        functionSource(identitySource, 'authorizeIdentityKey'),
        functionSource(identitySource, 'viewRecoveryPhrase'),
        functionSource(identitySource, 'exportIdentityPayload'),
        functionSource(identitySource, 'exportWarningForFormat'),
        functionSource(identitySource, 'openEncryptedIdentityExportModal'),
        functionSource(identitySource, 'exportIdentityBackup')
    ].join('\n'), context, { filename: 'identity.js' });
    return { context, seen, field, target };
}

function unauthorized(message) {
    const error = new Error(message);
    error.code = 'unauthorized';
    return error;
}

const settle = () => new Promise(resolve => setImmediate(resolve));
const opts = { title: 'Copy Secret', message: 'A <secret>.', confirmText: 'Copy', danger: true };

(async () => {
    // Without a PIN: a confirmation, then `confirmed`.
    let h = load();
    const sent = [];
    let result = await h.context.authorizeIdentityKey({ hash: HASH, passcode_protected: false }, opts,
        auth => { sent.push(auth); return Promise.resolve('released'); });
    assert.equal(result, 'released');
    assert.deepEqual(JSON.parse(JSON.stringify(sent)), [{ confirmed: true }]);
    assert.equal(h.seen.confirms[0].danger, true);
    assert.equal(h.seen.confirms[0].confirmText, 'Copy');

    h = load({ confirm: false });
    result = await h.context.authorizeIdentityKey({ hash: HASH }, opts,
        () => { throw new Error('declining sends nothing'); });
    assert.equal(result, null);

    // With a PIN: the sheet, which keeps a missing or wrong PIN inline.
    h = load();
    const pins = [];
    const replies = [unauthorized('Incorrect PIN'), 'released'];
    const pending = h.context.authorizeIdentityKey({ hash: HASH, passcode_protected: true }, opts, auth => {
        pins.push(auth.passcode);
        const reply = replies.shift();
        return reply instanceof Error ? Promise.reject(reply) : Promise.resolve(reply);
    });
    assert.equal(h.seen.confirms.length, 0);
    const sheet = h.seen.modals[0];
    assert.equal(sheet.title, 'Copy Secret');
    assert.match(sheet.body, /A &lt;secret&gt;\./);
    assert.equal(sheet.confirmClass, 'danger');
    assert.equal(h.field('identity-modal-confirm').textContent, 'Copy');
    assert.equal(sheet.onConfirm(), undefined, 'no PIN, no request');
    assert.equal(h.field('identity-key-error').textContent, 'Enter your PIN.');
    h.field('identity-key-pin').value = 'wrong pin';
    await sheet.onConfirm();
    assert.equal(h.field('identity-key-error').textContent, 'Incorrect PIN');
    assert.equal(h.seen.closed, 0, 'a wrong PIN keeps the sheet open');
    h.field('identity-key-pin').value = 'right pin';
    await sheet.onConfirm();
    await settle();
    assert.deepEqual(pins, ['wrong pin', 'right pin']);
    assert.equal(h.seen.closed, 1);
    assert.equal(await pending, 'released');

    // A recovery phrase: a confirmation without a PIN, the PIN sheet with one.
    h = load();
    h.context.viewRecoveryPhrase(h.target);
    await settle();
    assert.equal(h.seen.confirms.length, 1, 'an open phrase asks first');
    assert.deepEqual(h.seen.invoked, [['reveal_identity_mnemonic', { args: { hash: HASH, confirmed: true } }]]);
    assert.deepEqual(h.seen.shown, ['words']);

    h = load({ confirm: false });
    h.context.viewRecoveryPhrase(h.target);
    await settle();
    assert.deepEqual(h.seen.invoked, [], 'declining sends nothing');

    h = load({ protectedIdentity: true, replies: { reveal_identity_mnemonic: [unauthorized('Incorrect PIN')] } });
    h.context.viewRecoveryPhrase(h.target);
    await settle();
    let sheet2 = h.seen.modals[0];
    assert.equal(sheet2.title, 'Show the recovery phrase?');
    assert.match(sheet2.body, /Fir\u2019s PIN/);
    h.field('identity-key-pin').value = 'wrong pin';
    await sheet2.onConfirm();
    assert.equal(h.field('identity-key-error').textContent, 'Incorrect PIN');
    h.field('identity-key-pin').value = 'right pin';
    await sheet2.onConfirm();
    await settle();
    assert.deepEqual(h.seen.invoked.map(([, args]) => args), [
        { args: { hash: HASH, passcode: 'wrong pin' } },
        { args: { hash: HASH, passcode: 'right pin' } }
    ]);
    assert.deepEqual(h.seen.shown, ['words']);

    // The private key, each way.
    h = load({ format: 'reticulum-base32' });
    h.context.exportIdentityBackup(HASH);
    await settle();
    assert.equal(h.seen.confirms[0].danger, true);
    assert.deepEqual(h.seen.invoked, [
        ['api_export_identity_reticulum_base32', { hashHex: HASH, confirmed: true }]
    ]);
    assert.deepEqual(h.seen.saved, ['Reticulum base32 key']);

    h = load({ protectedIdentity: true, format: 'reticulum' });
    h.context.exportIdentityBackup(HASH);
    await settle();
    assert.match(h.seen.modals[0].body, /unencrypted private Reticulum identity key for Fir/);
    assert.equal(h.seen.modals[0].confirmClass, 'danger');
    h.field('identity-key-pin').value = 'right pin';
    await h.seen.modals[0].onConfirm();
    await settle();
    assert.deepEqual(h.seen.invoked, [
        ['api_export_identity_reticulum_base64', { hashHex: HASH, passcode: 'right pin' }]
    ]);
    assert.deepEqual(h.seen.saved, ['Reticulum identity file']);

    // A backup: its own PIN, plus the identity's or the sheet as confirmation.
    h = load({ format: 'ratspeak' });
    h.context.exportIdentityBackup(HASH);
    await settle();
    let backup = h.seen.modals[0];
    assert.doesNotMatch(backup.body, /Identity PIN/);
    assert.match(backup.body, /<label class="ui-label" for="identity-export-backup-pin">Backup PIN<\/label>/);
    assert.match(backup.body, /<label class="ui-label" for="identity-export-backup-pin-confirm">Confirm backup PIN<\/label>/);
    h.field('identity-export-backup-pin').value = 'backup pin';
    h.field('identity-export-backup-pin-confirm').value = 'backup pin';
    await backup.onConfirm();
    await settle();
    assert.deepEqual(h.seen.invoked, [
        ['api_export_identity_backup_base64', { hashHex: HASH, confirmed: true, passcode: 'backup pin' }]
    ]);
    assert.deepEqual(h.seen.saved, ['Ratspeak identity backup']);

    h = load({
        protectedIdentity: true,
        format: 'ratspeak',
        replies: { api_export_identity_backup_base64: [unauthorized('Incorrect PIN')] }
    });
    h.context.exportIdentityBackup(HASH);
    await settle();
    backup = h.seen.modals[0];
    assert.match(backup.body, /for="identity-export-identity-pin">Identity PIN<\/label>/);
    h.field('identity-export-backup-pin').value = 'backup pin';
    h.field('identity-export-backup-pin-confirm').value = 'backup pin';
    assert.equal(backup.onConfirm(), undefined, 'no identity PIN, no request');
    assert.equal(h.field('identity-export-backup-pin-error').textContent, "Enter this identity's PIN.");
    h.field('identity-export-identity-pin').value = 'wrong pin';
    await backup.onConfirm();
    assert.equal(h.field('identity-export-backup-pin-error').textContent, 'Incorrect PIN');
    assert.equal(h.seen.closed, 0);
    h.field('identity-export-identity-pin').value = 'right pin';
    await backup.onConfirm();
    await settle();
    assert.deepEqual(h.seen.invoked.map(([, args]) => args), [
        { hashHex: HASH, identityPasscode: 'wrong pin', passcode: 'backup pin' },
        { hashHex: HASH, identityPasscode: 'right pin', passcode: 'backup pin' }
    ]);
    assert.equal(h.seen.closed, 1);
    assert.deepEqual(h.seen.saved, ['Ratspeak identity backup']);

    console.log('Identity key authorization tests passed');
})().catch(error => {
    console.error(error);
    process.exit(1);
});
