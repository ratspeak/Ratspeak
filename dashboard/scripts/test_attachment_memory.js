#!/usr/bin/env node
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var dashboardRoot = path.join(__dirname, '..');
var lxmf = fs.readFileSync(path.join(dashboardRoot, 'static', 'js', 'lxmf.js'), 'utf8');
var state = fs.readFileSync(path.join(dashboardRoot, 'static', 'js', 'state.js'), 'utf8');

function functionSource(source, name) {
    var start = source.indexOf('function ' + name + '(');
    assert.notStrictEqual(start, -1, name + ' must exist');
    var brace = source.indexOf('{', start);
    var depth = 0;
    for (var i = brace; i < source.length; i++) {
        if (source[i] === '{') depth++;
        if (source[i] === '}') {
            depth--;
            if (depth === 0) return source.slice(start, i + 1);
        }
    }
    throw new Error('unterminated function ' + name);
}

assert(state.includes("window.RS.invoke('api_file_metadata'"),
    'downloads must inspect metadata before reading bytes');
assert(state.includes("window.RS.invoke('api_file_read_chunk'"),
    'downloads must use the bounded raw chunk command');
assert(state.includes('var _rsLargeFileDownloadActive = false;'));
assert(state.includes('var _rsSmallFileDownloadBytes = 0;'));
assert(state.includes('if (_rsFileDownloadInFlight[storedName])'));
assert(state.includes("window.RS.invoke('save_stored_attachment_native'"));
assert(state.includes("error.code !== 'native_save_unsupported'"));
var downloadStart = state.indexOf('window.RS.fileDownload = function');
var downloadEnd = state.indexOf('\nvar _rsAndroidFileSaveSeq', downloadStart);
assert(downloadStart >= 0 && downloadEnd > downloadStart, 'fileDownload source markers must exist');
assert(!state.slice(downloadStart, downloadEnd).includes('data_base64'),
    'the production download path must not retain whole-file base64');
assert(lxmf.includes("RS.invoke('begin_attachment_stage'"));
assert(lxmf.includes("RS.invoke('append_attachment_stage'"));
assert(lxmf.includes("RS.invoke('send_lxmf_with_staged_attachment'"));
assert(lxmf.includes('var _imageHydrationMax = 3;'));
assert(lxmf.includes("new IntersectionObserver"));
assert(lxmf.includes("return mobile ? (16 * 1024 * 1024) : (32 * 1024 * 1024);"));
assert(lxmf.includes('var _cacheMax = 8;'));
assert(lxmf.includes('var _conversationCacheMaxBytes = 2 * 1024 * 1024;'));
assert(lxmf.includes('function handleAttachmentMemoryPressure(critical)'));
assert(lxmf.includes("error.code !== 'attachment_image_unsafe'"));
assert(lxmf.includes("error.code !== 'attachment_image_memory_limit'"));
assert(lxmf.includes("inline_image: false, fell_back_to_file: true"),
    'unsupported image formats must remain transferable as ordinary files');
assert(state.includes("RS.listen('attachment_memory_pressure'"));
assert(!lxmf.includes("data_url: 'data:' + lxmfPendingFile.mime"),
    'optimistic image rows must not retain base64 data URLs');

var dimensionContext = {
    Uint8Array: Uint8Array,
    DataView: DataView,
    Math: Math,
    String: String,
    Error: Error,
    Promise: Promise,
};
vm.createContext(dimensionContext);
vm.runInContext(functionSource(lxmf, '_attachmentImageDimensions'), dimensionContext);

var revoked = [];
var cacheContext = {
    Math: Math,
    JSON: JSON,
    URL: { revokeObjectURL: function(url) { revoked.push(url); } },
    isTauriMobile: function() { return false; },
    isMobile: function() { return false; },
    _imageBlobUrlCache: {},
    _imageBlobUrlLru: [],
    _imageBlobUrlMax: 64,
    _imageBlobUrlBytes: 0,
    _imageCacheGeneration: 0,
    _imageHydrationQueue: [],
    _imageHydrationObserver: null,
};
vm.createContext(cacheContext);
['_imageBlobUrlBudget', '_evictImageBlobUrl', '_clearImageBlobUrlCache', '_rememberImageBlobUrl', '_makeImageBlobUrlRoom']
    .forEach(function(name) { vm.runInContext(functionSource(lxmf, name), cacheContext); });
assert.strictEqual(cacheContext._rememberImageBlobUrl('first', 'blob:first', { size: 20 * 1024 * 1024 }), true);
assert.strictEqual(cacheContext._rememberImageBlobUrl('second', 'blob:second', { size: 20 * 1024 * 1024 }), true);
assert.strictEqual(cacheContext._imageBlobUrlCache.first, undefined, 'byte budget must evict oldest image');
assert.deepStrictEqual(revoked, ['blob:first']);
cacheContext._clearImageBlobUrlCache();
assert.deepStrictEqual(revoked, ['blob:first', 'blob:second']);
cacheContext._rememberImageBlobUrl('cached', 'blob:cached', { size: 24 * 1024 * 1024 });
assert.strictEqual(cacheContext._makeImageBlobUrlRoom('incoming', 16 * 1024 * 1024), true);
assert.strictEqual(cacheContext._imageBlobUrlCache.cached, undefined,
    'hydration must evict before downloading the next large image');

var conversationContext = {
    Math: Math,
    JSON: JSON,
    Blob: Blob,
    _conversationCache: {},
    _conversationCacheSizes: {},
    _conversationCacheBytes: 0,
    _cacheLru: [],
    _cacheMax: 8,
    _conversationCacheMaxBytes: 2 * 1024 * 1024,
};
vm.createContext(conversationContext);
['_utf8ByteLength', 'cacheDel', 'cacheSet', 'cacheGet']
    .forEach(function(name) { vm.runInContext(functionSource(lxmf, name), conversationContext); });
for (var cacheIndex = 0; cacheIndex < 10; cacheIndex++) {
    conversationContext.cacheSet('conversation-' + cacheIndex, [{ content: 'bounded' }]);
}
assert.strictEqual(conversationContext._cacheLru.length, 8);
assert.strictEqual(conversationContext._conversationCache['conversation-0'], undefined);

function png(width, height) {
    var bytes = Buffer.alloc(24);
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]).copy(bytes, 0);
    bytes.writeUInt32BE(width, 16);
    bytes.writeUInt32BE(height, 20);
    return new Blob([bytes]);
}

(async function() {
    var dimensions = await dimensionContext._attachmentImageDimensions(png(4000, 3000));
    assert.strictEqual(dimensions.width, 4000);
    assert.strictEqual(dimensions.height, 3000);
    await assert.rejects(
        dimensionContext._attachmentImageDimensions(new Blob(['<svg></svg>'])),
        /Unsupported or malformed image/
    );
    console.log('Attachment memory tests passed');
})().catch(function(error) {
    console.error(error);
    process.exitCode = 1;
});
