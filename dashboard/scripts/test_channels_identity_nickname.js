#!/usr/bin/env node
// Deterministic tests: a renamed identity must never be offered as a channel
// nickname. The cached identity name is a pre-load hint only; the live
// identity and the retired-bookmark rule decide what we broadcast.

'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var channelsPath = path.join(__dirname, '..', 'static', 'js', 'channels.js');
var channelsSource = fs.readFileSync(channelsPath, 'utf8');

function sourceFunction(name, nextName) {
    var start = channelsSource.indexOf('function ' + name);
    var end = channelsSource.indexOf('\nfunction ' + nextName, start);
    assert(start !== -1 && end !== -1, name + ' must exist');
    return channelsSource.slice(start, end);
}

var defaultNickSource = sourceFunction('_channelsDefaultNickname', '_channelsHubName');

function evaluateDefaultNickname(options) {
    var context = {
        localStorage: {
            getItem: function() {
                return options.cachedName;
            }
        },
        activeIdentity: options.liveIdentity === undefined
            ? undefined
            : function() { return options.liveIdentity; }
    };
    vm.runInNewContext(
        defaultNickSource + '\nthis.defaultNickname = _channelsDefaultNickname;',
        context
    );
    return context.defaultNickname();
}

// The reported bug: rename saved and announced, but the connect sheet still
// offered the pre-rename name because the cached copy outranked the identity.
assert.strictEqual(
    evaluateDefaultNickname({
        cachedName: 'Old Name',
        liveIdentity: { display_name: 'New Name' }
    }),
    'New Name',
    'the live identity must win over a stale cached name'
);

assert.strictEqual(
    evaluateDefaultNickname({
        cachedName: 'Old Name',
        liveIdentity: { display_name: '', nickname: 'New Nick' }
    }),
    'New Nick',
    'a live nickname must win over a stale cached name'
);

// The cache stays useful strictly as a pre-load bootstrap hint.
assert.strictEqual(
    evaluateDefaultNickname({ cachedName: 'Cached', liveIdentity: null }),
    'Cached',
    'the cached name remains the fallback before the identity loads'
);
assert.strictEqual(
    evaluateDefaultNickname({ cachedName: '', liveIdentity: null }),
    'rat',
    'an unnamed identity falls back to the generic default'
);
assert.strictEqual(
    evaluateDefaultNickname({
        cachedName: 'Old Name',
        liveIdentity: { display_name: '   Spaced   ' }
    }),
    'Spaced',
    'the offered nickname is trimmed'
);

// Source contract: the live identity must be consulted before the cache, so a
// future edit cannot silently reintroduce the leak.
var liveIndex = defaultNickSource.indexOf('activeIdentity');
var cacheIndex = defaultNickSource.indexOf('localStorage');
assert(liveIndex !== -1 && cacheIndex !== -1, 'both sources must be present');
assert(
    liveIndex < cacheIndex,
    'the live identity must be read before the cached name'
);

// The rename handler must refresh the cached copy, otherwise every other
// consumer of the cache keeps showing the superseded name.
var identitySource = fs.readFileSync(
    path.join(__dirname, '..', 'static', 'js', 'identity.js'),
    'utf8'
);
var renameStart = identitySource.indexOf("RS.invoke('api_set_display_name'");
assert(renameStart !== -1, 'the rename call site must exist');
var renameBlock = identitySource.slice(renameStart, renameStart + 600);
assert(
    renameBlock.indexOf("localStorage.setItem('ratspeak_identity_name'") !== -1,
    'a successful rename must refresh the cached identity name'
);

console.log('channels identity nickname tests passed');
