#!/usr/bin/env node
'use strict';

// Run the production picker/uploader with real Blob slicing and Base64 bytes.
// The companion Rust attachment_staging tests exercise the actual IPC decoder.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const source = fs.readFileSync(path.join(__dirname, '../../dashboard/static/js/lxmf.js'), 'utf8');
const CHUNK_BYTES = 256 * 1024;

function extract(name) {
    const start = source.indexOf('function ' + name + '(');
    assert(start >= 0, name);
    const brace = source.indexOf('{', start);
    let depth = 0;
    for (let index = brace; index < source.length; index++) {
        if (source[index] === '{') depth++;
        if (source[index] === '}' && --depth === 0) return source.slice(start, index + 1);
    }
    throw Error('Unterminated function: ' + name);
}

function fixture(size, options = {}) {
    const data = Buffer.alloc(size);
    for (let index = 0; index < size; index++) data[index] = index * 31 % 251;
    const file = new Blob([data], { type: options.image ? 'image/jpeg' : 'application/octet-stream' });
    file.name = options.image ? 'photo.jpg' : 'data.bin';
    const calls = [], chunks = [], reads = [], choices = [];
    let written = 0, reading = false;
    const ctx = {
        lxmfActiveContact: 'aabbcc', lxmfPendingFile: null, _pendingAttachmentToken: 0,
        lxmfLimits: { max_attachment_bytes: 128000000 },
        document: { activeElement: null },
        renderPendingFile() {}, showToast() {}, prettySize: String,
        _imagePreviewUrl: () => 'blob:preview',
        _chooseImageSize: async () => { choices.push('size'); return options.cancelChoice ? null : 'medium'; },
        _chooseImageFileFallback: async () => { choices.push('file'); return 'file'; },
        clearPendingFile() { ctx._pendingAttachmentToken++; ctx.lxmfPendingFile = null; },
        FileReader: class {
            readAsDataURL(blob) {
                assert.equal(reading, false, 'only one chunk may be read at once');
                reading = true;
                reads.push(blob.size);
                blob.arrayBuffer().then(buffer => {
                    reading = false;
                    if (options.readFailure && reads.length === 2) return this.onerror();
                    this.result = 'data:application/octet-stream;base64,' + Buffer.from(buffer).toString('base64');
                    this.onload({ target: this });
                }).catch(error => { reading = false; this.error = error; this.onerror(); });
            }
        },
        RS: {
            composer: { dismissForReplacement: async () => {} },
            invoke: async (command, payload) => {
                calls.push(command);
                const args = payload.args;
                switch (command) {
                case 'begin_attachment_stage':
                    if (options.beginFailure) throw Error('begin failed');
                    assert.equal(args.declared_size, size);
                    assert.equal(args.dest_hash, 'aabbcc');
                    return { token: 'stage-token', chunk_bytes: options.chunkBytes || CHUNK_BYTES };
                case 'append_attachment_stage': {
                    if (options.appendFailure && chunks.length === 1) throw Error('append failed');
                    assert.equal(args.token, 'stage-token');
                    assert.equal(args.offset, written);
                    const decoded = Buffer.from(args.data_base64, 'base64');
                    assert(decoded.length > 0 && decoded.length <= CHUNK_BYTES);
                    assert.deepEqual(decoded, data.subarray(written, written + decoded.length));
                    const offset = written;
                    written += decoded.length;
                    chunks.push(decoded);
                    if (options.afterAppend) options.afterAppend(ctx);
                    return { written: options.ack ? options.ack(written, offset, chunks.length) : written };
                }
                case 'cancel_attachment_stage':
                    assert.equal(payload.token, 'stage-token');
                    return { cancelled: true };
                case 'inspect_image_attachment_stage':
                    assert.equal(written, size, 'inspection must follow complete staging');
                    return { disposition: options.disposition || 'still', should_prompt: size > 1000000 };
                case 'prepare_image_attachment_stage':
                    assert.equal(args.profile, size > 1000000 ? 'medium' : 'actual');
                    assert.equal(args.automatic, size <= 1000000);
                    return { file_name: 'prepared.jpg', mime: 'image/jpeg', size: 1234, profile: args.profile };
                case 'mark_image_attachment_stage_as_file': return {};
                default: throw Error('Unexpected command: ' + command);
                }
            },
        },
    };
    vm.createContext(ctx);
    for (const name of ['_canonicalConversationHash', '_pendingAttachmentName', '_readBlobBase64',
        '_stageAttachmentBlob', '_looksLikeImageAttachment', '_attachmentCancelledError',
        '_isCurrentPendingAttachment', '_imageProfileLabel', '_finishPendingImageAsFile',
        '_stageSelectedImage', 'handleFileSelected']) {
        vm.runInContext(extract(name), ctx);
    }
    return {
        ctx, file, data, calls, chunks, reads, choices,
        stage: () => ctx._stageAttachmentBlob(file, file.name, file.type, !!options.image, 'AABBCC'),
        pick() {
            const input = { files: [file], value: 'selected' };
            ctx.handleFileSelected(input);
            assert.equal(input.value, '');
            return ctx.lxmfPendingFile;
        },
    };
}

(async () => {
    for (const size of [1, 2, 3, CHUNK_BYTES - 2, CHUNK_BYTES - 1, CHUNK_BYTES,
        CHUNK_BYTES + 1, CHUNK_BYTES * 2, CHUNK_BYTES * 2 + 1, CHUNK_BYTES * 2 + 2,
        CHUNK_BYTES * 2 + 3, 1000000, 1000001, 6000000]) {
        const f = fixture(size);
        assert.equal(await f.stage(), 'stage-token');
        assert.deepEqual(Buffer.concat(f.chunks), f.data);
        assert.equal(f.chunks.length, Math.ceil(size / CHUNK_BYTES));
        assert(!f.calls.includes('cancel_attachment_stage'));
        assert(f.reads.every(bytes => bytes <= CHUNK_BYTES), 'never read the whole large file');
    }
    for (const advertised of [32768, 512 * 1024]) {
        const f = fixture(600000, { chunkBytes: advertised });
        await f.stage();
        assert.equal(f.reads[0], Math.min(advertised, CHUNK_BYTES));
        assert.deepEqual(Buffer.concat(f.chunks), f.data);
    }
    for (const options of [{ readFailure: true }, { appendFailure: true }]) {
        const f = fixture(CHUNK_BYTES * 3, options);
        await assert.rejects(f.stage());
        assert.equal(f.calls.filter(name => name === 'cancel_attachment_stage').length, 1);
        assert.equal(f.reads.length, 2, 'a failed chunk must stop further reads');
    }
    const failedBegin = fixture(1, { beginFailure: true });
    await assert.rejects(failedBegin.stage(), /begin failed/);
    assert.deepEqual(failedBegin.calls, ['begin_attachment_stage']);

    // Acknowledgements must match this exact chunk, not merely fall somewhere
    // in the file. Otherwise stale replies can loop, or skip unread file bytes.
    for (const ack of [() => undefined, () => NaN, () => -1, end => end + 0.5,
        end => end + 1, () => CHUNK_BYTES * 3, (_end, offset) => offset,
        (end, _offset, count) => count === 2 ? CHUNK_BYTES : end]) {
        const f = fixture(CHUNK_BYTES * 3, { ack });
        await assert.rejects(f.stage(), /invalid offset/);
        assert.equal(f.calls.filter(name => name === 'cancel_attachment_stage').length, 1);
        assert(f.reads.length <= 2);
    }

    const file = fixture(CHUNK_BYTES * 2 + 1);
    const pendingFile = file.pick();
    assert.equal(await pendingFile.stage_promise, 'stage-token');
    assert.deepEqual(Buffer.concat(file.chunks), file.data);
    assert.equal(pendingFile.staging_token, 'stage-token');

    for (const size of [CHUNK_BYTES, 1000000, 1000001]) {
        const f = fixture(size, { image: true });
        const pending = f.pick();
        assert.equal(await pending.stage_promise, 'stage-token');
        assert.deepEqual(Buffer.concat(f.chunks), f.data);
        assert.deepEqual(f.choices, size > 1000000 ? ['size'] : []);
        assert(f.calls.includes('prepare_image_attachment_stage'));
        assert.equal(pending.inline_image, true);
        assert.equal(pending.preparing, false);
        assert.equal(pending.size, 1234);
    }
    for (const disposition of ['animated', 'unsupported', 'too_large']) {
        const f = fixture(CHUNK_BYTES + 1, { image: true, disposition });
        const pending = f.pick();
        assert.equal(await pending.stage_promise, 'stage-token');
        assert.deepEqual(f.choices, ['file']);
        assert(f.calls.includes('mark_image_attachment_stage_as_file'));
        assert(!f.calls.includes('prepare_image_attachment_stage'));
        assert.equal(pending.inline_image, false);
        assert.equal(pending.size, f.file.size);
    }
    for (const options of [{ cancelChoice: true }, { afterAppend: ctx => { ctx.lxmfActiveContact = 'another'; } }]) {
        const f = fixture(1000001, { image: true, ...options });
        const pending = f.pick();
        assert.equal(await pending.stage_promise, null);
        assert(f.calls.includes('cancel_attachment_stage'));
        assert(!f.calls.includes('prepare_image_attachment_stage'));
    }
    console.log('Attachment staging: full chunks, tails, byte fidelity, bounded reads, failures, acknowledgements and picker preparation passed');
})().catch(error => { console.error(error); process.exitCode = 1; });
