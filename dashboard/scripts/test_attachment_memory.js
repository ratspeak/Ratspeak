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
assert(lxmf.includes("RS.invoke('inspect_image_attachment_stage'"));
assert(lxmf.includes("RS.invoke('prepare_image_attachment_stage'"));
assert(lxmf.includes("RS.invoke('mark_image_attachment_stage_as_file'"));
assert(lxmf.includes('var _imageHydrationMax = 3;'));
assert(lxmf.includes("new IntersectionObserver"));
assert(lxmf.includes("return mobile ? (16 * 1024 * 1024) : (32 * 1024 * 1024);"));
assert(lxmf.includes('var _cacheMax = 8;'));
assert(lxmf.includes('var _conversationCacheMaxBytes = 2 * 1024 * 1024;'));
assert(lxmf.includes('function handleAttachmentMemoryPressure(critical)'));
assert(!lxmf.includes('createImageBitmap('),
    'large source photos must not decode in the WebView');
assert(!lxmf.includes("document.createElement('canvas')"),
    'image transformation must stay in bounded Rust staging');
assert(lxmf.includes("inspection.disposition !== 'still'"));
assert(lxmf.includes("choice !== 'file'"),
    'unsupported and animated images require explicit file fallback');
assert(lxmf.includes('pendingFile.destination === lxmfActiveContact'),
    'preparation must be fenced to the originating conversation');
assert(lxmf.includes('_canonicalConversationHash(pendingAttachment.destination) !== targetHash'),
    'a prepared attachment must not drift into another conversation');
assert(state.includes("RS.listen('attachment_memory_pressure'"));
assert(!lxmf.includes("data_url: 'data:' + lxmfPendingFile.mime"),
    'optimistic image rows must not retain base64 data URLs');

var choicePayload = null;
var choiceContext = {
    prettySize: function(bytes) { return bytes + ' B'; },
    rsChoice: function(payload) { choicePayload = payload; return Promise.resolve('medium'); },
};
vm.createContext(choiceContext);
['_pendingAttachmentName', '_imageProfileHint', '_imageProfileLabel', '_chooseImageSize']
    .forEach(function(name) { vm.runInContext(functionSource(lxmf, name), choiceContext); });

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
['_canonicalConversationHash', '_utf8ByteLength', 'cacheDel', 'cacheSet', 'cacheGet']
    .forEach(function(name) { vm.runInContext(functionSource(lxmf, name), conversationContext); });
for (var cacheIndex = 0; cacheIndex < 10; cacheIndex++) {
    conversationContext.cacheSet('conversation-' + cacheIndex, [{ content: 'bounded' }]);
}
assert.strictEqual(conversationContext._cacheLru.length, 8);
assert.strictEqual(conversationContext._conversationCache['conversation-0'], undefined);
conversationContext.cacheSet('AABBCC', [{ content: 'canonical' }]);
assert.strictEqual(conversationContext.cacheGet('aabbcc')[0].content, 'canonical',
    'conversation caches must not split case-equivalent destination hashes');

(async function() {
    var selected = await choiceContext._chooseImageSize(
        { name: 'mountain.jpg', size: 6400000 },
        {
            width: 4032,
            height: 3024,
            source_bytes: 6400000,
            options: [
                { profile: 'small', label: 'Small', max_edge: 960, estimated_bytes: 240000 },
                { profile: 'medium', label: 'Medium', max_edge: 1600, estimated_bytes: 720000, recommended: true },
                { profile: 'large', label: 'Large', max_edge: 2560, estimated_bytes: 1900000 },
                { profile: 'actual', label: 'Actual size', estimated_bytes: 6400000 },
            ],
        }
    );
    assert.strictEqual(selected, 'medium');
    assert.strictEqual(choicePayload.sheetClass, 'image-size-sheet');
    assert.strictEqual(choicePayload.choices.length, 4);
    assert.deepStrictEqual(choicePayload.choices.map(function(choice) { return choice.meta; }), [
        '~240000 B', '~720000 B', '~1900000 B', '~6400000 B'
    ]);
    assert.strictEqual(choicePayload.choices[1].recommended, true);
    assert.strictEqual(choicePayload.summary.secondary, '4032 × 3024 · 6400000 B');
    console.log('Attachment memory tests passed');
})().catch(function(error) {
    console.error(error);
    process.exitCode = 1;
});
