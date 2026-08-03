#!/usr/bin/env node
// Deterministic regressions for the session-scoped public room directory.

'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var dashboardRoot = path.join(__dirname, '..');
var channelsSource = fs.readFileSync(
    path.join(dashboardRoot, 'static', 'js', 'channels.js'),
    'utf8'
);
var navSource = fs.readFileSync(
    path.join(dashboardRoot, 'static', 'js', 'nav.js'),
    'utf8'
);

function sourceFunction(name, nextName) {
    var start = channelsSource.indexOf('function ' + name);
    var end = channelsSource.indexOf('\nfunction ' + nextName, start);
    assert(start !== -1 && end !== -1, name + ' must exist');
    return channelsSource.slice(start, end);
}

function deferred() {
    var resolve;
    var reject;
    var promise = new Promise(function(ok, fail) {
        resolve = ok;
        reject = fail;
    });
    return { promise: promise, resolve: resolve, reject: reject };
}

function FakeElement(tagName) {
    this.tagName = String(tagName || 'div').toUpperCase();
    this.children = [];
    this.dataset = {};
    this.attributes = {};
    this.listeners = {};
    this.className = '';
    this.hidden = false;
    this.disabled = false;
    this._text = '';
}

Object.defineProperty(FakeElement.prototype, 'textContent', {
    get: function() { return this._text; },
    set: function(value) {
        this._text = String(value == null ? '' : value);
        if (this._text === '') this.children = [];
    }
});

FakeElement.prototype.appendChild = function(child) {
    this.children.push(child);
    return child;
};
FakeElement.prototype.setAttribute = function(name, value) {
    this.attributes[name] = String(value);
};
FakeElement.prototype.addEventListener = function(name, handler) {
    this.listeners[name] = handler;
};
FakeElement.prototype.click = function() {
    if (this.listeners.click) {
        this.listeners.click({
            preventDefault: function() {},
            stopPropagation: function() {}
        });
    }
};

function descendants(root) {
    var result = [];
    (root.children || []).forEach(function(child) {
        result.push(child);
        result = result.concat(descendants(child));
    });
    return result;
}

function textTree(root) {
    return [root.textContent].concat((root.children || []).map(textTree)).join(' ');
}

async function main() {
    var nativeDirectory = deferred();
    var invokeCount = 0;
    var applied = [];
    var refreshContext = {
        channelsSnapshot: {
            generation: 1,
            revision: 1,
            phase: 'active',
            hub: { destination_hash: 'hub-a' },
            directory: {
                phase: 'idle',
                rooms: [],
                refreshed_at_ms: null
            }
        },
        _channelsDirectoryRefreshPromise: null,
        _channelsDirectoryRequestSeq: 0,
        CHANNEL_DIRECTORY_STALE_AFTER_MS: 300000,
        _channelsIsConnected: function() { return true; },
        _channelsDirectoryNeedsRefresh: function() { return true; },
        channelsApplySnapshot: function(snapshot) {
            applied.push(snapshot);
            refreshContext.channelsSnapshot = snapshot;
            return true;
        },
        RS: {
            invoke: function(command) {
                invokeCount += 1;
                assert.strictEqual(command, 'refresh_channel_directory');
                return nativeDirectory.promise;
            }
        },
        Promise: Promise
    };
    vm.runInNewContext(
        sourceFunction('channelsRefreshDirectory', 'channelsLoadSavedRooms'),
        refreshContext,
        { filename: 'channels-directory-refresh.js' }
    );

    var first = refreshContext.channelsRefreshDirectory(false);
    var duplicate = refreshContext.channelsRefreshDirectory(false);
    assert.strictEqual(first, duplicate, 'concurrent view refreshes must coalesce');
    assert.strictEqual(invokeCount, 1);
    nativeDirectory.resolve({
        generation: 1,
        revision: 2,
        phase: 'active',
        hub: { destination_hash: 'hub-a' },
        directory: { phase: 'loading', rooms: [], refreshed_at_ms: null }
    });
    await first;
    assert.strictEqual(applied.length, 1);

    var lateNative = deferred();
    refreshContext.RS.invoke = function(command) {
        assert.strictEqual(command, 'refresh_channel_directory');
        return lateNative.promise;
    };
    var late = refreshContext.channelsRefreshDirectory(true);
    refreshContext.channelsSnapshot = {
        generation: 1,
        revision: 3,
        phase: 'active',
        hub: { destination_hash: 'hub-b' },
        directory: { phase: 'idle', rooms: [], refreshed_at_ms: null }
    };
    refreshContext._channelsDirectoryRequestSeq += 1;
    refreshContext._channelsDirectoryRefreshPromise = null;
    lateNative.resolve({
        generation: 1,
        revision: 4,
        phase: 'active',
        hub: { destination_hash: 'hub-a' },
        directory: { phase: 'ready', rooms: [{ name: 'wrong-hub' }] }
    });
    await late;
    assert.strictEqual(applied.length, 1,
        'a late response from the previous hub must be discarded');
    assert.strictEqual(refreshContext.channelsSnapshot.hub.destination_hash, 'hub-b');

    var staleContext = {
        channelsSnapshot: {
            phase: 'active',
            directory: { phase: 'idle', refreshed_at_ms: null }
        },
        CHANNEL_DIRECTORY_STALE_AFTER_MS: 300000,
        _channelsIsConnected: function() { return true; },
        Date: Date
    };
    vm.runInNewContext(
        sourceFunction('_channelsDirectoryNeedsRefresh', '_channelsShortHash') +
            '\nthis.needsRefresh = _channelsDirectoryNeedsRefresh;',
        staleContext,
        { filename: 'channels-directory-staleness.js' }
    );
    assert.strictEqual(staleContext.needsRefresh(), true);
    staleContext.channelsSnapshot.directory = {
        phase: 'ready',
        refreshed_at_ms: Date.now()
    };
    assert.strictEqual(staleContext.needsRefresh(), false);
    staleContext.channelsSnapshot.directory.refreshed_at_ms = Date.now() - 300001;
    assert.strictEqual(staleContext.needsRefresh(), true);
    staleContext.channelsSnapshot.directory.phase = 'error';
    assert.strictEqual(staleContext.needsRefresh(), false,
        'errors wait for an explicit retry instead of background hammering');

    var elements = {
        'channels-list': new FakeElement('div'),
        'channels-list-label': new FakeElement('span'),
        'channels-join-btn': new FakeElement('button')
    };
    var refreshForces = [];
    var joinPrefill = null;
    var renderContext = {
        document: {
            createElement: function(tag) { return new FakeElement(tag); }
        },
        channelsSnapshot: {
            phase: 'active',
            hub: { destination_hash: 'hub-a' },
            rooms: [{ name: 'general', phase: 'joined' }],
            directory: {
                phase: 'ready',
                rooms: [
                    { name: 'general', topic: 'Already joined' },
                    { name: 'saved', topic: 'Already saved' },
                    { name: 'public-room', topic: 'Field coordination' }
                ],
                complete: false,
                omitted_count: 2,
                last_error: null
            }
        },
        channelsSavedRooms: [
            { hub_destination_hash: 'hub-a', room_name: 'saved' }
        ],
        channelsRoomIndex: [],
        channelsHistorySelection: null,
        channelsActiveRoom: 'general',
        _channelsEl: function(id) { return elements[id] || null; },
        _channelsIsConnected: function() { return true; },
        _channelsBuildRoomRow: function(room) {
            var row = new FakeElement('button');
            row.className = 'test-owned-room';
            row.dataset.room = room.name;
            return row;
        },
        _channelsRoomIcon: function() { return '<svg></svg>'; },
        channelsRefreshDirectory: function(force) { refreshForces.push(force); },
        channelsOpenJoinSheet: function(room) { joinPrefill = room; },
        Object: Object,
        Number: Number,
        Array: Array
    };
    vm.runInNewContext(
        sourceFunction('_channelsRoomDisplayName', '_channelsTimelineHubName') + '\n' +
            sourceFunction('_channelsRenderList', '_channelsListSection') + '\n' +
            sourceFunction('_channelsListSection', '_channelsEmptyList') + '\n' +
            sourceFunction('_channelsBuildDirectoryRoomRow', '_channelsBuildRoomRow'),
        renderContext,
        { filename: 'channels-directory-render.js' }
    );
    renderContext._channelsRenderList();
    assert.strictEqual(elements['channels-list-label'].textContent, 'Active');
    assert.strictEqual(elements['channels-join-btn'].textContent, 'Join');

    var listSections = elements['channels-list'].children.filter(function(element) {
        return element.className.indexOf('channels-list-section') === 0;
    }).map(function(element) {
        return element.children[0].textContent;
    });
    assert.strictEqual(listSections.join('|'), 'History|Discover',
        'disconnected rooms must be separated from the Joined list');

    var all = descendants(elements['channels-list']);
    var directoryRows = all.filter(function(element) {
        return element.className.indexOf('channel-directory-room') !== -1;
    });
    assert.strictEqual(directoryRows.length, 1,
        'joined and saved rooms must not be duplicated in the public directory');
    assert.strictEqual(directoryRows[0].dataset.room, 'public-room');
    assert(textTree(directoryRows[0]).indexOf('Field coordination') !== -1);
    directoryRows[0].click();
    assert.strictEqual(joinPrefill, 'public-room',
        'directory selection must preview the existing join sheet');

    var refreshButtons = all.filter(function(element) {
        return element.className === 'channels-list-section-action';
    });
    assert.strictEqual(refreshButtons.length, 1);
    refreshButtons[0].click();
    assert.deepStrictEqual(refreshForces, [true]);
    assert(textTree(elements['channels-list']).indexOf(
        'The hub kept its response within one constrained packet'
    ) !== -1, 'hub-declared truncation must stay visible');

    renderContext.channelsSnapshot.directory.rooms = [
        { name: 'general', topic: 'Already joined' },
        { name: 'saved', topic: 'Already saved' }
    ];
    renderContext.channelsSnapshot.directory.complete = true;
    renderContext.channelsSnapshot.directory.omitted_count = 0;
    renderContext._channelsRenderList();
    assert(textTree(elements['channels-list']).indexOf(
        'No discoverable channels found.'
    ) !== -1, 'an empty discovery result must use concise neutral copy');

    renderContext.channelsSnapshot.rooms = [];
    renderContext._channelsRenderList();
    assert(textTree(elements['channels-list']).indexOf(
        'No currently active channels'
    ) !== -1, 'a connected hub with no joined rooms must show a quiet Active placeholder');
    assert(elements['channels-list'].children.some(function(element) {
        return element.className === 'channel-active-empty';
    }), 'the Active placeholder must remain visually distinct from directory status cards');

    assert(navSource.indexOf('channelsRefreshDirectory(false);') !== -1,
        'entering the Channels view must request an idle or stale directory');
    assert(channelsSource.indexOf('service_model_version: 3') !== -1);
    assert(channelsSource.indexOf('channelsOpenJoinSheet(room.name)') !== -1);
    assert(channelsSource.indexOf("RS.invoke('refresh_channel_directory')") !== -1);

    console.log('channel public directory tests passed');
}

main().catch(function(error) {
    console.error(error);
    process.exitCode = 1;
});
