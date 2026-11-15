#!/usr/bin/env node
// The public servers the Network screen lists and joins: three Ratspeak servers and the
// community list, one list of endpoints shared with the Rust transport limits, matched for TCP
// and Backbone clients alike. Runs the shipped matching and join code against stubs.

'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var repoRoot = path.join(__dirname, '..', '..');
function read(relative) {
    return fs.readFileSync(path.join(repoRoot, relative), 'utf8');
}

var modalsSource = read('dashboard/static/js/modals.js');

function sourceRange(source, startMarker, endMarker) {
    var start = source.indexOf(startMarker);
    var end = source.indexOf(endMarker, start);
    assert(start !== -1 && end !== -1, startMarker + ' range must exist');
    return source.slice(start, end);
}

function functionSource(source, name) {
    var start = source.indexOf('function ' + name + '(');
    assert.notStrictEqual(start, -1, name + ' must exist');
    var depth = 0;
    for (var index = source.indexOf('{', start); index < source.length; index++) {
        if (source[index] === '{') depth++;
        if (source[index] === '}') {
            depth--;
            if (depth === 0) return source.slice(start, index + 1);
        }
    }
    throw new Error('unterminated function ' + name);
}

function createHarness(opts) {
    opts = opts || {};
    var live = {};
    var invoked = [];
    var toasts = [];
    var renders = 0;
    var context = {
        console: console,
        Promise: Promise,
        window: {},
        getInterfaceLiveStatus: function(name) { return live[name] || null; },
        refreshConfigInterfaces: function() {},
        showToast: function(message, tone) { toasts.push([message, tone]); },
        RS: {
            networkView: { render: function() { renders++; } },
            invoke: function(name, args) {
                invoked.push({ name: name, args: args });
                return opts.reject ? Promise.reject(new Error(opts.reject)) : Promise.resolve(null);
            }
        }
    };
    vm.createContext(context);
    vm.runInContext(
        sourceRange(modalsSource, '// The Network screen lists these', 'function _trapFocus') +
            sourceRange(modalsSource, 'function _connectMatchingPublicInterface', '// ------------------------------------------------------------------ network name and passphrase') +
            'var _connectAwaiting = null, _hostAwaiting = null;\n' +
            functionSource(modalsSource, 'connectionDialogOperation'),
        context,
        { filename: 'modals-public-servers.js' }
    );
    return {
        context: context,
        live: live,
        invoked: invoked,
        toasts: toasts,
        renders: function() { return renders; },
        server: function(id) {
            return context.PUBLIC_TCP_SERVERS.filter(function(server) { return server.id === id; })[0];
        },
        state: function(id, ifaces) { return context._publicServerState(this.server(id), ifaces || {}); }
    };
}

function plain(value) { return JSON.parse(JSON.stringify(value)); }

var COMMUNITY = [
    ['Apollo RNS', 'apollo-rns', 'rns.not-a-number.io', 4242, 'Belgium'],
    ['Beleth', 'beleth', 'rns.beleth.net', 4242, ''],
    ['MichMesh', 'michmesh', 'rns.michmesh.net', 7822, 'Michigan, US'],
    ['One-Big-Network', 'one-big-network', 'rns.one-big.network', 4242, 'Germany'],
    ['RMAP', 'rmap', 'rmap.world', 4242, 'Italy'],
    ['Swiss RNS', 'swiss-rns', 'swiss-rns.cl0ud.ch', 4242, 'Switzerland'],
    ['UTN Oregon', 'utn-oregon', 'rns.utn.lol', 4965, 'Oregon, US'],
    ['Washmesh', 'washmesh', 'reticulum.washmesh.net', 7242, 'Washington, US']
];

function testList() {
    var h = createHarness();
    var servers = h.context.PUBLIC_TCP_SERVERS;
    assert.deepStrictEqual(Array.prototype.map.call(servers.filter(function(server) {
        return server.group === 'official';
    }), function(server) { return [server.id, server.name, server.tone]; }), [
        ['ratspeak-ruby', 'Ruby', 'ruby'], ['ratspeak-emerald', 'Emerald', 'emerald'], ['ratspeak-diamond', 'Diamond', 'diamond']
    ], 'the three Ratspeak servers come first, each with its tone');
    COMMUNITY.forEach(function(row) {
        var server = h.server(row[1]);
        assert(server, row[1] + ' must keep its id so saved connections still match');
        assert.strictEqual(server.group, 'community');
        assert.strictEqual(server.name, row[0]);
        assert.strictEqual(server.host + ':' + server.port, row[2] + ':' + row[3]);
        assert.strictEqual(server.place || '', row[4]);
    });
    assert.strictEqual(servers.length, 11);
}

function testEndpoints() {
    var h = createHarness();
    var servers = h.context.PUBLIC_TCP_SERVERS;
    var ids = {};
    var keys = {};
    var endpoints = [];
    servers.forEach(function(server) {
        assert(!ids[server.id], 'duplicate id ' + server.id);
        ids[server.id] = true;
        [{ host: server.host, port: server.port }].concat(server.aliases || []).forEach(function(endpoint) {
            assert(Number.isInteger(endpoint.port) && endpoint.port > 0 && endpoint.port < 65536,
                server.id + ' must state its port; a missing one would fall back to 4242');
            var key = h.context._tcpServerKey(endpoint.host, endpoint.port);
            assert(!keys[key], 'duplicate endpoint ' + key);
            keys[key] = true;
            endpoints.push({ host: endpoint.host, port: endpoint.port, id: server.id });
            assert(h.context._publicServerMatchesEndpoint(server, endpoint.host.toUpperCase(), String(endpoint.port)));
        });
    });
    var emerald = h.server('ratspeak-emerald');
    assert(h.context._publicServerMatchesEndpoint(emerald, 'rns.ratspeak.org', 4242), 'the Emerald alias stays Emerald');
    assert(h.context._publicServerMatchesEndpoint(emerald, 'https://2.Ratspeak.org./', 4242),
        'a host is matched in the form the Rust limit matches it');
    assert(!h.context._publicServerMatchesEndpoint(h.server('michmesh'), 'rns.michmesh.net', 4242),
        'a server is its host and its own port');

    // The transport limits count public servers from one list in Rust, which
    // the commands and the startup check both read.
    var runtime = read('crates/ratspeak-runtime/src/lib.rs');
    var block = sourceRange(runtime, 'const PUBLIC_TCP_ENDPOINTS', '];');
    endpoints.forEach(function(endpoint) {
        var tuple = '("' + endpoint.host + '", ' + endpoint.port + ', "' + endpoint.id + '")';
        assert(block.indexOf(tuple) !== -1, 'the runtime must list ' + tuple);
    });
    assert.strictEqual(block.split('\n    ("').length - 1, endpoints.length,
        'the runtime must list exactly the dashboard endpoints');
    assert.strictEqual(runtime.split('TCP_ENDPOINTS: &[').length - 1, 1, 'one list in the runtime');
    assert(runtime.indexOf('const PUBLIC_TCP_CLIENT_GROUPS: [&str; 2] = ["tcp_client", "backbone_client"];') !== -1,
        'a server counts whether a TCP client or a Backbone client reaches it');
    var commands = read('crates/ratspeak-tauri/src/commands/interfaces.rs');
    assert.strictEqual(commands.indexOf('TCP_ENDPOINTS: &['), -1, 'the commands keep no second list');
}

// A public server saved as a Backbone client is the same server; one online wins.
function testMatching() {
    var h = createHarness();
    h.live['Backbone to Ruby'] = { online: false };
    var ifaces = {
        tcp_client: [],
        backbone_client: [
            { name: 'Backbone to Ruby', target_host: '1.RATSPEAK.org.', target_port: '4141' },
            { name: 'Private', target_host: 'hub.example', target_port: '4141' }
        ],
        // A listener connects to nothing.
        backbone_server: [{ name: 'Listener', target_host: '3.ratspeak.org', target_port: '4343' }]
    };
    var ruby = h.state('ratspeak-ruby', ifaces);
    assert(ruby.configured && !ruby.connected);
    assert.strictEqual(ruby.match.ifaceType, 'backbone_client');
    assert.strictEqual(h.state('ratspeak-diamond', ifaces).match, null);

    [['Emerald', 'Backbone to Emerald'], ['Backbone to Emerald', 'Emerald']].forEach(function(pair) {
        var both = createHarness();
        both.live[pair[0]] = { online: true };
        both.live[pair[1]] = { online: false };
        var state = both.state('ratspeak-emerald', {
            tcp_client: [{ name: 'Emerald', target_host: '2.ratspeak.org', target_port: 4242 }],
            backbone_client: [{ name: 'Backbone to Emerald', target_host: 'rns.ratspeak.org', target_port: 4242 }]
        });
        assert(state.connected, pair[0] + ' is online');
        assert.strictEqual(state.match.iface.name, pair[0]);
    });
}

function testJoin() {
    var h = createHarness();
    var michmesh = h.server('michmesh');
    h.context.connectPublicServer(michmesh);
    assert.deepStrictEqual(plain(h.invoked), [{
        name: 'add_tcp_connection',
        args: { args: { host: 'rns.michmesh.net', port: 7822, name: 'MichMesh', ifac_enabled: false, ifac_network_name: '', ifac_passphrase: '' } }
    }], 'a public server joins as a TCP client on its own port, which Backbone servers accept too');
    assert.strictEqual(h.state('michmesh').pending, true, 'the Network screen shows it joining');
    assert.strictEqual(h.renders(), 1);

    // A failed join, reported later, clears the joining row; a success leaves it until connected.
    h.context.connectionDialogOperation({ operation: 'add_tcp', done: true, error: null });
    assert.strictEqual(h.state('michmesh').pending, true);
    h.context.connectionDialogOperation({ operation: 'add_tcp', done: true, error: 'refused' });
    assert.strictEqual(h.state('michmesh').pending, false);
    assert.strictEqual(h.renders(), 2);

    var refused = createHarness({ reject: 'The public server limit is reached.' });
    refused.context.connectPublicServer(michmesh);
    return Promise.resolve().then(function() {}).then(function() {
        assert.strictEqual(refused.state('michmesh').pending, false, 'a refused join clears at once');
        assert.deepStrictEqual(refused.toasts, [['The public server limit is reached.', 'toast-error']]);
    });
}

function testResume() {
    var h = createHarness();
    h.context.resumePublicServerInterface(h.server('ratspeak-ruby'), {
        iface: { name: 'Backbone to Ruby' }, ifaceType: 'backbone_client'
    });
    assert.deepStrictEqual(plain(h.invoked), [{
        name: 'resume_interface', args: { args: { name: 'Backbone to Ruby', iface_type: 'backbone_client' } }
    }], 'a saved server resumes instead of being added twice');
    assert.strictEqual(h.state('ratspeak-ruby').pending, true);
}

testList();
testEndpoints();
testMatching();
testResume();
testJoin().then(function() {
    console.log('connect public servers tests passed');
}).catch(function(err) {
    console.error(err);
    process.exit(1);
});
