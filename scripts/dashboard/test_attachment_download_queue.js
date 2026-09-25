#!/usr/bin/env node
'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const root = path.join(__dirname, '../../dashboard/static/js');
const state = fs.readFileSync(path.join(root, 'state.js'), 'utf8');
const lxmf = fs.readFileSync(path.join(root, 'lxmf.js'), 'utf8');
function extract(source, marker) {
    const start = source.indexOf(marker); assert(start >= 0, marker);
    let depth = 0;
    for (let i = source.indexOf('{', start); i < source.length; i++) {
        if (source[i] === '{') depth++;
        if (source[i] === '}' && --depth === 0) return source.slice(start, i + 1);
    }
    throw Error('Unterminated ' + marker);
}
function deferred() { let resolve; const promise = new Promise(r => { resolve = r; }); return { promise, resolve }; }
async function settle() { for (let i = 0; i < 300; i++) await Promise.resolve(); }
function fixture() {
    const calls = [], errors = [], revoked = [], hooks = {}, sizes = {};
    let urlId = 0, active = 0, peak = 0;
    const ctx = {
        Blob, Uint8Array, ArrayBuffer,
        URL: { createObjectURL: () => 'blob:' + (++urlId), revokeObjectURL: url => revoked.push(url) },
        _rsFileDownloadInFlight: {}, _rsLargeFileDownloadActive: false,
        _rsLargeFileDownloadQueue: Promise.resolve(), _rsSmallFileDownloadBytes: 0,
        _imageHydrationInFlight: {}, _imageCacheGeneration: 0, _imageBlobUrlCache: {},
        _imageBlobUrlLru: [], _imageBlobUrlBytes: 0, _imageBlobUrlMax: 64,
        _imageHydrationActive: 0, _imageHydrationMax: 3, _imageHydrationQueue: [], _imageHydrationObserver: null,
        isTauriMobile: () => true, isMobile: () => true,
        RS: {
            fileMetadata: async name => ({ size: sizes[name] ?? 1200000, chunk_bytes: 262144,
                inline_image: true, mime: 'image/jpeg', filename: name }),
            invoke: async (command, args) => {
                assert.equal(command, 'api_file_read_chunk');
                calls.push(args);
                assert(args.length > 0 && args.length <= 262144);
                active++; peak = Math.max(peak, active);
                try {
                    if (hooks[args.storedName]) await hooks[args.storedName](args);
                    return new Uint8Array(args.length).fill(args.offset % 251);
                } finally { active--; }
            },
        },
    };
    ctx.window = ctx;
    vm.createContext(ctx);
    for (const name of ['_rsRawIpcBytes', '_rsWithLargeFileDownload']) {
        vm.runInContext(extract(state, 'function ' + name + '('), ctx);
    }
    vm.runInContext(extract(state, 'window.RS.fileDownload = function('), ctx);
    for (const name of ['_imageBlobUrlBudget', '_evictImageBlobUrl', '_rememberImageBlobUrl',
        '_getImageDownloadFile', '_makeImageBlobUrlRoom', '_imageHydrationPromise',
        '_drainImageHydrationQueue', '_queueImageHydration', '_clearImageBlobUrlCache']) {
        vm.runInContext(extract(lxmf, 'function ' + name + '('), ctx);
    }
    function hydrate(name) {
        const attrs = { 'data-stored-name': name };
        const img = { isConnected: true, classList: { add: () => errors.push(name) },
            getAttribute: key => attrs[key], setAttribute: (key, value) => { attrs[key] = value; }, closest: () => null };
        ctx._queueImageHydration(img, name);
        return img;
    }
    return { ctx, calls, errors, revoked, hooks, sizes, hydrate, peak: () => peak };
}
(async () => {
    const f = fixture(), gate = deferred();
    f.hooks.first = () => gate.promise;
    const images = ['first', 'second', 'third', 'fourth'].map(f.hydrate);
    await settle();
    assert.deepEqual(f.calls.map(call => call.storedName), ['first'], 'queued images must not allocate chunks yet');
    assert.equal(f.ctx._imageHydrationActive, 3);
    gate.resolve(); await settle();
    assert(images.every(img => img.src), 'all visible large images should load without a busy failure');
    assert.deepEqual(f.errors, []);
    assert.equal(f.peak(), 1);
    assert.equal(f.ctx._imageHydrationActive, 0);
    assert.equal(f.ctx._rsLargeFileDownloadActive, false);
    assert.equal(f.ctx._imageBlobUrlBytes, 4 * 1200000);
    for (const name of ['first', 'second', 'third', 'fourth']) {
        const file = f.ctx._getImageDownloadFile(name);
        assert.equal(file.size, 1200000);
        const data = new Uint8Array(await file.blob.arrayBuffer());
        for (const call of f.calls.filter(call => call.storedName === name)) {
            assert(data.subarray(call.offset, call.offset + call.length).every(byte => byte === call.offset % 251));
        }
    }

    const mixed = fixture(), blocked = deferred();
    mixed.hooks.large = () => blocked.promise;
    const large = mixed.ctx.RS.fileDownload('large');
    assert.equal(mixed.ctx.RS.fileDownload('large'), large, 'same-file consumers share one download');
    const queued = mixed.ctx.RS.fileDownload('queued');
    assert.equal(mixed.ctx.RS.fileDownload('queued'), queued, 'single-flight includes queued work');
    mixed.sizes.small = 1048575;
    mixed.sizes.empty = 0;
    assert.equal((await mixed.ctx.RS.fileDownload('small')).size, 1048575);
    assert.equal((await mixed.ctx.RS.fileDownload('empty')).size, 0);
    assert(!mixed.calls.some(call => call.storedName === 'queued'));
    assert.equal(mixed.ctx._rsSmallFileDownloadBytes, 0);
    blocked.resolve(); await Promise.all([large, queued]);
    assert.equal(mixed.ctx._rsLargeFileDownloadActive, false);
    assert.equal(Object.keys(mixed.ctx._rsFileDownloadInFlight).length, 0);

    const budget = fixture(), smallRead = deferred(), smallDownloads = [];
    for (let i = 0; i < 8; i++) {
        const name = 'small-' + i;
        budget.sizes[name] = 1048575;
        budget.hooks[name] = () => smallRead.promise;
        smallDownloads.push(budget.ctx.RS.fileDownload(name));
    }
    budget.sizes.excess = 1048575;
    await assert.rejects(budget.ctx.RS.fileDownload('excess'), error => error.code === 'attachment_memory_pressure');
    assert.equal(budget.ctx._rsSmallFileDownloadBytes, 8 * 1048575);
    smallRead.resolve(); await Promise.all(smallDownloads);
    assert.equal(budget.ctx._rsSmallFileDownloadBytes, 0);
    assert.equal((await budget.ctx.RS.fileDownload('excess')).size, 1048575);

    const failed = fixture();
    failed.hooks.broken = async () => { throw Error('read failed'); };
    const rejection = assert.rejects(failed.ctx.RS.fileDownload('broken'), /read failed/);
    const good = failed.ctx.RS.fileDownload('good');
    await rejection; assert.equal((await good).size, 1200000);
    delete failed.hooks.broken;
    assert.equal((await failed.ctx.RS.fileDownload('broken')).size, 1200000, 'failed single-flight must allow retry');

    const truncated = fixture();
    const read = truncated.ctx.RS.invoke;
    truncated.ctx.RS.invoke = (command, args) => args.storedName === 'short'
        ? Promise.resolve(new Uint8Array(args.length - 1)) : read(command, args);
    const badLength = assert.rejects(truncated.ctx.RS.fileDownload('short'), /truncated/);
    const next = truncated.ctx.RS.fileDownload('next');
    await badLength; assert.equal((await next).size, 1200000);

    const stale = fixture(), oldRead = deferred();
    stale.hooks.old = () => oldRead.promise;
    const oldImages = ['old', 'waiting', 'also-waiting'].map(stale.hydrate);
    await settle(); stale.ctx._clearImageBlobUrlCache();
    oldRead.resolve(); await settle();
    assert(oldImages.every(img => !img.src), 'stale results cannot repopulate the display');
    assert.deepEqual(stale.errors, []);
    assert.equal(stale.revoked.length, 3);
    assert.equal(stale.ctx._imageBlobUrlBytes, 0);
    assert.equal(stale.ctx._rsLargeFileDownloadActive, false);
    const current = stale.hydrate('current'); await settle(); assert(current.src);

    console.log('Attachment downloads: queued hydration, bounded reads, mixed sizes, single-flight, failure/retry and stale cache generations passed');
})().catch(error => { console.error(error); process.exitCode = 1; });
