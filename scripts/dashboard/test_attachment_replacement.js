#!/usr/bin/env node
'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const source = fs.readFileSync(path.join(__dirname, '../../dashboard/static/js/lxmf.js'), 'utf8');
function extract(name) {
    const start = source.indexOf('function ' + name + '('); assert(start >= 0, name);
    let depth = 0;
    for (let i = source.indexOf('{', start); i < source.length; i++) {
        if (source[i] === '{') depth++;
        if (source[i] === '}' && --depth === 0) return source.slice(start, i + 1);
    }
}
function deferred() { let resolve; const promise = new Promise(r => { resolve = r; }); return { promise, resolve }; }
async function settle() { for (let i = 0; i < 40; i++) await Promise.resolve(); }
function fixture() {
    const stages = new Map(), calls = [], revoked = [], hooks = {};
    let sequence = 0;
    const ctx = {
        lxmfActiveContact: 'peer', lxmfPendingFile: null, _pendingAttachmentToken: 0,
        _attachmentRetirement: Promise.resolve(), lxmfLimits: { max_attachment_bytes: 128000000 },
        document: { getElementById: () => null, activeElement: null },
        renderPendingFile() {}, showToast() {}, prettySize: String,
        URL: { revokeObjectURL: url => revoked.push(url) },
        _imagePreviewUrl: () => 'blob:preview',
        _chooseImageSize: () => hooks.choice ? hooks.choice.promise : Promise.resolve('medium'),
        _chooseImageFileFallback: () => Promise.resolve('file'),
        FileReader: class {
            readAsDataURL(blob) {
                blob.arrayBuffer().then(bytes => this.onload({ target: { result: 'data:;base64,' + Buffer.from(bytes).toString('base64') } }));
            }
        },
        RS: { composer: { dismissForReplacement: async () => {} }, invoke: async (command, input) => {
            calls.push(command);
            const args = input.args;
            if (command === 'begin_attachment_stage') {
                assert(![...stages.values()].some(stage => stage.size > 1048575), 'replacement must await old admission release');
                const token = 'stage-' + (++sequence);
                stages.set(token, { size: args.declared_size, written: 0 });
                if (hooks.begin) await hooks.begin.promise;
                return { token, chunk_bytes: 262144 };
            }
            if (command === 'append_attachment_stage') {
                if (hooks.append) await hooks.append.promise;
                const stage = stages.get(args.token); assert(stage);
                assert.equal(args.offset, stage.written);
                stage.written += Buffer.from(args.data_base64, 'base64').length;
                return { written: stage.written };
            }
            if (command === 'cancel_attachment_stage') {
                if (hooks.cancel) await hooks.cancel.promise;
                stages.delete(input.token); return { cancelled: true };
            }
            if (command === 'inspect_image_attachment_stage') return { disposition: 'still', should_prompt: true };
            if (command === 'prepare_image_attachment_stage') {
                if (hooks.prepare) await hooks.prepare.promise;
                return { file_name: 'photo.jpg', mime: 'image/jpeg', size: 1200000, profile: args.profile };
            }
            throw Error(command);
        } },
    };
    ctx.window = ctx;
    vm.createContext(ctx);
    for (const name of ['_canonicalConversationHash', '_pendingAttachmentName', '_looksLikeImageAttachment',
        '_attachmentCancelledError', '_readBlobBase64', '_stageAttachmentBlob', 'clearPendingFile',
        '_isCurrentPendingAttachment', '_imageProfileLabel', '_stageSelectedImage', 'attachSharedImage', 'handleFileSelected']) {
        vm.runInContext(extract(name), ctx);
    }
    function pick(image = false) {
        const file = new Blob([new Uint8Array(1200000)], { type: image ? 'image/jpeg' : 'application/pdf' });
        file.name = image ? 'photo.jpg' : 'file.pdf';
        ctx.handleFileSelected({ files: [file], value: 'picked' });
        return ctx.lxmfPendingFile;
    }
    return { ctx, pick, stages, calls, hooks, revoked };
}
(async () => {
    const ready = fixture();
    const old = ready.pick(); await old.stage_promise;
    ready.hooks.cancel = deferred();
    const replacement = ready.pick(); await settle();
    assert.equal(ready.calls.filter(x => x === 'begin_attachment_stage').length, 1);
    ready.hooks.cancel.resolve();
    assert(await replacement.stage_promise);
    assert.equal(ready.stages.size, 1);
    assert(old.cancelled);
    await ready.ctx.clearPendingFile(); assert.equal(ready.stages.size, 0);

    for (const phase of ['begin', 'append']) {
        const f = fixture(); f.hooks[phase] = deferred();
        const a = f.pick();
        // FileReader uses Blob.arrayBuffer, which can resolve on a later turn.
        while (!f.calls.includes(phase === 'begin' ? 'begin_attachment_stage' : 'append_attachment_stage')) {
            await new Promise(resolve => setImmediate(resolve));
        }
        const b = f.pick(); const c = f.pick(); await settle();
        assert.equal(f.calls.filter(x => x === 'begin_attachment_stage').length, 1);
        f.hooks[phase].resolve();
        assert.equal(await a.stage_promise, null);
        assert.equal(await b.stage_promise, null);
        assert(await c.stage_promise);
        assert.equal(f.calls.filter(x => x === 'begin_attachment_stage').length, 2);
        assert.equal(f.stages.size, 1);
        await f.ctx.clearPendingFile(); assert.equal(f.stages.size, 0);
    }

    const chooser = fixture(); chooser.hooks.choice = deferred();
    const image = chooser.pick(true); await image.source_stage_promise; await settle();
    const file = chooser.pick(); assert(await file.stage_promise, 'retired chooser must not block new file');
    chooser.hooks.choice.resolve('medium'); assert.equal(await image.stage_promise, null);
    assert(!chooser.calls.includes('prepare_image_attachment_stage'));
    await chooser.ctx.clearPendingFile();

    const prepared = fixture(); const photo = prepared.pick(true); await photo.stage_promise;
    const next = prepared.pick(); assert(await next.stage_promise);
    assert.deepEqual(prepared.revoked, ['blob:preview']);
    await prepared.ctx.clearPendingFile();

    const preparing = fixture(); preparing.hooks.prepare = deferred();
    const latePhoto = preparing.pick(true);
    while (!preparing.calls.includes('prepare_image_attachment_stage')) {
        await new Promise(resolve => setImmediate(resolve));
    }
    const afterPhoto = preparing.pick(); assert(await afterPhoto.stage_promise);
    preparing.hooks.prepare.resolve();
    assert.equal(await latePhoto.stage_promise, null);
    assert.equal(preparing.ctx.lxmfPendingFile, afterPhoto, 'late preparation cannot replace the new draft');
    assert.equal(preparing.stages.size, 1);
    await preparing.ctx.clearPendingFile();

    const removed = fixture(); removed.hooks.begin = deferred();
    const first = removed.pick(); await settle();
    const queued = removed.pick();
    const cleared = removed.ctx.clearPendingFile();
    removed.hooks.begin.resolve();
    await Promise.all([first.stage_promise, queued.stage_promise, cleared]);
    assert.equal(removed.calls.filter(x => x === 'begin_attachment_stage').length, 1);
    assert.equal(removed.stages.size, 0, 'clearing a queued replacement retires both selections');

    const native = fixture(); const original = native.pick(); await original.stage_promise;
    native.hooks.cancel = deferred(); let nativeStarted = false;
    const shared = native.ctx.attachSharedImage({ name: 'shared.jpg', size: 1200000, mime: 'image/jpeg' }, () => {
        nativeStarted = true; assert.equal(native.stages.size, 0);
        native.stages.set('native', { size: 1200000 }); return Promise.resolve('native');
    });
    await settle(); assert.equal(nativeStarted, false);
    native.hooks.cancel.resolve(); assert.equal(await shared.stage_promise, 'native');
    await native.ctx.clearPendingFile(); assert.equal(native.stages.size, 0);

    const detached = fixture(); const sending = detached.pick(); await sending.stage_promise;
    detached.ctx.handleFileSelected({ files: [], value: '' });
    assert.equal(detached.ctx.lxmfPendingFile, sending, 'closing picker preserves existing attachment');
    sending.detached_for_send = true;
    await detached.ctx.clearPendingFile();
    assert.equal(sending.cancelled, false);
    assert.equal(detached.stages.size, 1, 'send owner retains staging');
    assert(!detached.calls.includes('cancel_attachment_stage'));
    console.log('Attachment replacement: ordered cleanup, late replies, rapid picks, chooser retirement, native shares and send ownership passed');
})().catch(error => { console.error(error); process.exitCode = 1; });
