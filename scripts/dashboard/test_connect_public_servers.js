#!/usr/bin/env node
// The Connect to Network sheet's Public tab: three Ratspeak servers up front,
// community servers inside a closed disclosure, and one list of endpoints
// shared with the Rust transport limits, matched for TCP and Backbone clients
// alike. Runs the shipped card and disclosure code against a small DOM model
// that parses the markup it writes.

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
var peerDetailsSource = read('dashboard/static/js/peer_details.js');
var stateSource = read('dashboard/static/js/state.js');
var viewsCss = read('dashboard/static/css/10-views.css');
var modalsCss = read('dashboard/static/css/08-modals.css');

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

// ---------------------------------------------------------------- DOM model

function FakeElement(tagName) {
    this.tagName = String(tagName || 'div').toUpperCase();
    this.children = [];
    this.parentNode = null;
    this.dataset = {};
    this.attributes = {};
    this.listeners = {};
    this.style = {};
    this.disabled = false;
    this.inert = false;
    this.value = '';
    this._text = '';
}

function decodeEntities(text) {
    return text.replace(/&amp;/g, '&').replace(/&lt;/g, '<').replace(/&gt;/g, '>')
        .replace(/&quot;/g, '"').replace(/&#39;/g, "'");
}

// Parses the well-formed markup the dashboard writes: tags, attributes, text.
function parseInto(parent, html) {
    var stack = [parent];
    var pattern = /<\/([a-z0-9]+)>|<([a-z0-9]+)((?:\s+[a-z-]+(?:="[^"]*")?)*)\s*(\/?)>|([^<]+)/gi;
    var match;
    while ((match = pattern.exec(html))) {
        var top = stack[stack.length - 1];
        if (match[1]) {
            stack.pop();
        } else if (match[2]) {
            var element = new FakeElement(match[2]);
            var attributes = match[3].match(/[a-z-]+(?:="[^"]*")?/gi) || [];
            attributes.forEach(function(pair) {
                var equals = pair.indexOf('=');
                var name = equals === -1 ? pair : pair.slice(0, equals);
                element.setAttribute(name, equals === -1 ? '' : decodeEntities(pair.slice(equals + 2, -1)));
            });
            top.appendChild(element);
            if (!match[4]) stack.push(element);
        } else if (match[5].trim()) {
            var text = new FakeElement('#text');
            text._text = decodeEntities(match[5]);
            top.appendChild(text);
        }
    }
    assert.strictEqual(stack.length, 1, 'markup must be balanced: ' + html.slice(0, 80));
}

Object.defineProperty(FakeElement.prototype, 'innerHTML', {
    get: function() { return ''; },
    set: function(html) {
        this.children.forEach(function(child) { child.parentNode = null; });
        this.children = [];
        this._text = '';
        parseInto(this, String(html));
    }
});
Object.defineProperty(FakeElement.prototype, 'textContent', {
    get: function() {
        return this._text + this.children.map(function(child) {
            return child.textContent;
        }).join('');
    },
    set: function(value) {
        this.children = [];
        this._text = String(value == null ? '' : value);
    }
});
Object.defineProperty(FakeElement.prototype, 'className', {
    get: function() { return this.attributes['class'] || ''; },
    set: function(value) { this.attributes['class'] = String(value); }
});
Object.defineProperty(FakeElement.prototype, 'id', {
    get: function() { return this.attributes.id || ''; },
    set: function(value) { this.attributes.id = String(value); }
});
Object.defineProperty(FakeElement.prototype, 'title', {
    get: function() { return this.attributes.title || ''; },
    set: function(value) { this.attributes.title = String(value); }
});

FakeElement.prototype.hasClass = function(name) {
    return this.className.split(/\s+/).indexOf(name) !== -1;
};
FakeElement.prototype.appendChild = function(child) {
    child.parentNode = this;
    this.children.push(child);
    return child;
};
FakeElement.prototype.setAttribute = function(name, value) {
    value = String(value);
    this.attributes[name] = value;
    if (name.indexOf('data-') === 0) {
        this.dataset[name.slice(5).replace(/-([a-z])/g, function(_m, ch) {
            return ch.toUpperCase();
        })] = value;
    }
    if (name === 'inert') this.inert = true;
};
FakeElement.prototype.getAttribute = function(name) {
    return Object.prototype.hasOwnProperty.call(this.attributes, name)
        ? this.attributes[name] : null;
};
FakeElement.prototype.addEventListener = function(name, handler) {
    (this.listeners[name] = this.listeners[name] || []).push(handler);
};
FakeElement.prototype.contains = function(other) {
    for (var node = other; node; node = node.parentNode) {
        if (node === this) return true;
    }
    return false;
};
FakeElement.prototype.focus = function() { FakeElement.active = this; };
FakeElement.prototype.click = function() {
    if (this.disabled) return;
    (this.listeners.click || []).slice().forEach(function(handler) { handler({}); });
};

function matches(element, selector) {
    if (selector.charAt(0) === '#') return element.id === selector.slice(1);
    if (selector.charAt(0) === '.') return element.hasClass(selector.slice(1));
    var attribute = selector.match(/^\[([a-z-]+)="([^"]*)"\]$/);
    if (attribute) {
        var key = attribute[1].slice(5).replace(/-([a-z])/g, function(_m, ch) {
            return ch.toUpperCase();
        });
        return element.dataset[key] === attribute[2] ||
            element.getAttribute(attribute[1]) === attribute[2];
    }
    return element.tagName === selector.toUpperCase();
}
FakeElement.prototype.querySelectorAll = function(selector) {
    var found = [];
    (function walk(node) {
        node.children.forEach(function(child) {
            if (matches(child, selector)) found.push(child);
            walk(child);
        });
    })(this);
    return found;
};
FakeElement.prototype.querySelector = function(selector) {
    return this.querySelectorAll(selector)[0] || null;
};

// ---------------------------------------------------------------- harness

function createHarness() {
    FakeElement.active = null;
    var elements = { 'public-server-list': new FakeElement('div') };
    var live = {};
    var invoked = [];
    var quickConnects = [];
    var context = {
        console: console,
        Promise: Promise,
        window: {},
        _connectEditContext: null,
        document: {
            get activeElement() { return FakeElement.active; },
            createElement: function(tag) { return new FakeElement(tag); },
            // Unknown ids resolve to a throwaway element so the sheet opener can run whole.
            getElementById: function(id) {
                return elements[id] || (elements[id] = new FakeElement('div'));
            },
            querySelector: function() { return new FakeElement('div'); },
            querySelectorAll: function() { return []; }
        },
        getInterfaceLiveStatus: function(name) { return live[name] || null; },
        quickConnect: function(host, port, name, opts) {
            quickConnects.push({ host: host, port: port, name: name, opts: opts });
        },
        refreshConfigInterfaces: function() {},
        showToast: function() {},
        RS: {
            invoke: function(name, args) {
                invoked.push({ name: name, args: args });
                return Promise.resolve(null);
            },
            ui: { openExistingSheet: function() {} }
        },
        _normaliseConnectEditContext: function() { return null; },
        setBottomSheetTitleWithIcon: function() {},
        setConnectTab: function() {},
        _ifacPopulate: function() {},
        _ifaceString: function() { return ''; },
        _syncConnectAdvancedVisibility: function() {},
        loadConnectionHistory: function() {}
    };
    vm.createContext(context);
    vm.runInContext(
        functionSource(stateSource, 'escapeHtml') + '\n' +
            sourceRange(peerDetailsSource, 'var PEER_ICON_ATTRS', 'var PEER_ICON_COPY') +
            sourceRange(peerDetailsSource, 'var PEER_ICON_CHEVRON', 'var PEER_ICON_MESSAGE') +
            sourceRange(modalsSource, '// group \'official\'', 'function _setConnectSubmitBase') +
            sourceRange(modalsSource, 'function _connectMatchingPublicInterface', 'function _ifaceIfacNetworkName') +
            functionSource(modalsSource, 'openConnectModal'),
        context,
        { filename: 'modals-public-servers.js' }
    );

    var list = elements['public-server-list'];
    var harness = {
        context: context,
        list: list,
        live: live,
        invoked: invoked,
        quickConnects: quickConnects,
        render: function(ifaces) {
            harness.ifaces = ifaces || harness.ifaces || {};
            context.window._hubInterfacesData = harness.ifaces;
            context.renderPublicTcpServers(harness.ifaces);
        },
        upFront: function() {
            return list.children.filter(function(child) { return child.hasClass('public-server-card'); });
        },
        box: function() { return list.querySelector('.public-server-more'); },
        toggle: function() { return list.querySelector('.public-server-more-toggle'); },
        region: function() { return list.querySelector('.public-server-more-region'); },
        community: function() {
            return harness.region().querySelectorAll('.public-server-card');
        },
        card: function(id) { return list.querySelector('[data-public-server="' + id + '"]'); },
        isOpen: function() {
            var open = harness.toggle().getAttribute('aria-expanded') === 'true';
            assert.strictEqual(harness.box().getAttribute('data-open'), open ? 'true' : 'false',
                'the slide and the button state must agree');
            assert.strictEqual(harness.region().inert, !open,
                'closed cards must be out of the tab order, open ones in it');
            return open;
        }
    };
    return harness;
}

function names(cards) {
    return cards.map(function(card) {
        return card.querySelector('.public-server-name').textContent;
    });
}

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

function testGrouping() {
    var h = createHarness();
    h.render({});
    assert.deepStrictEqual(names(h.upFront()), ['Ruby', 'Emerald', 'Diamond'],
        'only the three Ratspeak servers are shown up front');
    h.upFront().forEach(function(card) {
        assert.deepStrictEqual(
            card.querySelectorAll('.public-server-tag').map(function(tag) { return tag.textContent; }),
            ['OFFICIAL']
        );
        assert.strictEqual(card.disabled, false);
        assert(card.getAttribute('aria-label').indexOf('Connect ') === 0);
    });
    assert.strictEqual(h.list.children[h.list.children.length - 1], h.box(),
        'the disclosure sits under the three cards');

    var toggle = h.toggle();
    assert.strictEqual(toggle.tagName, 'BUTTON');
    assert.strictEqual(toggle.getAttribute('type'), 'button');
    assert.strictEqual(toggle.querySelector('span').textContent, 'Public servers');
    assert.strictEqual(toggle.getAttribute('aria-controls'), h.region().id);
    assert.strictEqual(h.region().getAttribute('aria-labelledby'), toggle.id);
    assert(toggle.querySelector('.peer-details-chevron'), 'the chevron is the peer Details chevron');
    assert.strictEqual(h.isOpen(), false, 'closed by default');

    assert.strictEqual(h.region().querySelector('.public-server-more-note').textContent,
        'Run by the community. Their operators can see your IP address.');
    assert.deepStrictEqual(names(h.community()), COMMUNITY.map(function(row) { return row[0]; }),
        'community servers are listed alphabetically by name');
    h.community().forEach(function(card, index) {
        assert.strictEqual(card.dataset.publicServer, COMMUNITY[index][1]);
        assert.strictEqual(card.querySelectorAll('.public-server-tag').length, 0,
            'the group says what they are; a card carries no tag');
        assert.strictEqual(card.textContent.indexOf('OFFICIAL'), -1);
        var place = card.querySelector('.public-server-place');
        assert.strictEqual(place ? place.textContent : '', COMMUNITY[index][4]);
        assert(card.hasClass('public-server-card--community'));
        assert.strictEqual(card.querySelector('.public-server-mark').textContent,
            COMMUNITY[index][0].charAt(0));
    });

    // The order is the names', not the data's.
    h.context.PUBLIC_TCP_SERVERS.reverse();
    h.render({});
    assert.deepStrictEqual(names(h.community()), COMMUNITY.map(function(row) { return row[0]; }));
    assert.strictEqual(names(h.upFront()).length, 3);
}

function testToggle() {
    var h = createHarness();
    h.render({});
    h.toggle().click();
    assert.strictEqual(h.isOpen(), true);
    h.render({});
    assert.strictEqual(h.isOpen(), true, 'a re-render keeps the choice made in this sheet');
    h.toggle().click();
    assert.strictEqual(h.isOpen(), false);

    // A rebuild returns keyboard focus to the control that held it.
    h.toggle().focus();
    h.render({});
    assert.strictEqual(FakeElement.active, h.toggle());

    // The choice lasts for one opening of the sheet and is stored nowhere.
    h.toggle().click();
    assert.strictEqual(h.isOpen(), true);
    h.context.openConnectModal();
    h.render({});
    assert.strictEqual(h.isOpen(), false, 'each opening starts closed');
    var disclosure = sourceRange(modalsSource, 'function _syncCommunityServers', 'function renderPublicTcpServers');
    assert.strictEqual(disclosure.indexOf('localStorage'), -1);
    assert.strictEqual(disclosure.indexOf('peer-details-toggle'), -1,
        'the peer Details click handler persists its state and must not adopt this row');
}

function testOpensForSavedOrConnected() {
    var h = createHarness();
    var saved = { tcp_client: [{ name: 'RMAP', target_host: 'rmap.world', target_port: '4242' }] };
    h.live.RMAP = { online: false };
    h.render(saved);
    assert.strictEqual(h.isOpen(), true, 'a saved community server opens the list by itself');
    var rmap = h.card('rmap');
    assert(rmap.hasClass('is-added') && !rmap.hasClass('is-connected'));
    assert.strictEqual(rmap.disabled, false);
    rmap.click();
    assert.deepStrictEqual(JSON.parse(JSON.stringify(h.invoked)), [{
        name: 'resume_interface', args: { args: { name: 'RMAP', iface_type: 'tcp_client' } }
    }], 'an already added server resumes instead of being added twice');
    assert.strictEqual(h.quickConnects.length, 0);

    h.toggle().click();
    assert.strictEqual(h.isOpen(), false, 'the row still closes when asked');
    h.render(saved);
    assert.strictEqual(h.isOpen(), false);

    var connected = createHarness();
    connected.live.MichMesh = { online: true };
    connected.render({
        tcp_client: [{ name: 'MichMesh', target_host: 'RNS.MichMesh.net', target_port: 7822 }]
    });
    assert.strictEqual(connected.isOpen(), true, 'a connected community server opens the list');
    var mich = connected.card('michmesh');
    assert(mich.hasClass('is-connected'));
    assert.strictEqual(mich.disabled, true);
    assert.strictEqual(mich.getAttribute('aria-label'), 'Connected MichMesh');

    // A saved Ratspeak server, even under its alias, opens nothing.
    var official = createHarness();
    official.render({
        tcp_client: [{ name: 'Emerald', target_host: 'rns.ratspeak.org', target_port: 4242 }]
    });
    assert(official.card('ratspeak-emerald').hasClass('is-added'));
    assert.strictEqual(official.isOpen(), false);
}

// A public server saved as a Backbone client is the same server.
function testSavedAsBackbone() {
    var h = createHarness();
    h.live['Backbone to Ruby'] = { online: false };
    h.render({
        tcp_client: [],
        backbone_client: [
            { name: 'Backbone to Ruby', target_host: '1.RATSPEAK.org.', target_port: '4141' },
            { name: 'Private', target_host: 'hub.example', target_port: '4141' }
        ],
        // A listener connects to nothing.
        backbone_server: [{ name: 'Listener', target_host: '3.ratspeak.org', target_port: '4343' }]
    });
    var ruby = h.card('ratspeak-ruby');
    assert(ruby.hasClass('is-added') && !ruby.hasClass('is-connected'));
    assert(!h.card('ratspeak-diamond').hasClass('is-added'));
    ruby.click();
    assert.deepStrictEqual(JSON.parse(JSON.stringify(h.invoked)), [{
        name: 'resume_interface', args: { args: { name: 'Backbone to Ruby', iface_type: 'backbone_client' } }
    }], 'the saved Backbone interface resumes; no TCP copy is added beside it');
    assert.strictEqual(h.quickConnects.length, 0);

    var connected = createHarness();
    connected.live['Backbone to RMAP'] = { online: true };
    connected.render({
        backbone_client: [{ name: 'Backbone to RMAP', target_host: 'rmap.world', target_port: 4242 }]
    });
    assert.strictEqual(connected.isOpen(), true, 'a community server connected by Backbone opens the list');
    var rmap = connected.card('rmap');
    assert(rmap.hasClass('is-connected'));
    assert.strictEqual(rmap.disabled, true);

    // Saved both ways, the one that is online decides, whichever type it is.
    [['Emerald', 'Backbone to Emerald'], ['Backbone to Emerald', 'Emerald']].forEach(function(pair) {
        var both = createHarness();
        both.live[pair[0]] = { online: true };
        both.live[pair[1]] = { online: false };
        both.render({
            tcp_client: [{ name: 'Emerald', target_host: '2.ratspeak.org', target_port: 4242 }],
            backbone_client: [{ name: 'Backbone to Emerald', target_host: 'rns.ratspeak.org', target_port: 4242 }]
        });
        assert(both.card('ratspeak-emerald').hasClass('is-connected'), pair[0] + ' is online');
    });
    var paused = createHarness();
    paused.render({
        tcp_client: [{ name: 'Emerald', target_host: '2.ratspeak.org', target_port: 4242 }],
        backbone_client: [{ name: 'Backbone to Emerald', target_host: 'rns.ratspeak.org', target_port: 4242 }]
    });
    paused.card('ratspeak-emerald').click();
    assert.deepStrictEqual(JSON.parse(JSON.stringify(paused.invoked)), [{
        name: 'resume_interface', args: { args: { name: 'Emerald', iface_type: 'tcp_client' } }
    }], 'with neither online the first saved resumes, and only that one');
}

function testConnect() {
    var h = createHarness();
    h.render({});
    h.toggle().click();
    h.card('michmesh').click();
    assert.deepStrictEqual(JSON.parse(JSON.stringify(h.quickConnects)), [{
        host: 'rns.michmesh.net', port: 7822, name: 'MichMesh', opts: { publicServer: true }
    }], 'a preset connects on its own port as a TCP client');
    var pending = h.card('michmesh');
    assert(pending.hasClass('is-pending'));
    assert.strictEqual(pending.disabled, true);
    assert.strictEqual(pending.getAttribute('aria-label'), 'Connecting... MichMesh');
    assert.strictEqual(h.isOpen(), true);

    h.card('ratspeak-ruby').click();
    assert.strictEqual(h.quickConnects[1].port, 4141);
    assert.strictEqual(h.quickConnects[1].host, '1.ratspeak.org');

    // A connection in progress shows even if the sheet was reopened meanwhile.
    h.context._connectCommunityOpen = null;
    h.context._connectPendingPublicServerKey = 'rns.utn.lol:4965';
    h.render({});
    assert.strictEqual(h.isOpen(), true);
    assert(h.card('utn-oregon').hasClass('is-pending'));
}

function testEndpoints() {
    var h = createHarness();
    var servers = h.context.PUBLIC_TCP_SERVERS;
    assert.strictEqual(servers.length, 11);
    var ids = {};
    var keys = {};
    var endpoints = [];
    servers.forEach(function(server) {
        assert(!ids[server.id], 'duplicate id ' + server.id);
        ids[server.id] = true;
        assert(server.group === 'official' || server.group === 'community', server.id);
        [{ host: server.host, port: server.port }].concat(server.aliases || []).forEach(function(endpoint) {
            assert(Number.isInteger(endpoint.port) && endpoint.port > 0 && endpoint.port < 65536,
                server.id + ' must state its port; a missing one would fall back to 4242');
            var key = h.context._tcpServerKey(endpoint.host, endpoint.port);
            assert(!keys[key], 'duplicate endpoint ' + key);
            keys[key] = true;
            endpoints.push({ host: endpoint.host, port: endpoint.port, id: server.id });
            assert.strictEqual(h.context._isPublicTcpServer(endpoint.host.toUpperCase(), String(endpoint.port)), true);
        });
    });
    COMMUNITY.forEach(function(row) {
        var server = servers.filter(function(candidate) { return candidate.id === row[1]; })[0];
        assert(server, row[1] + ' must keep its id so saved connections still match');
        assert.strictEqual(server.group, 'community');
        assert.strictEqual(server.host + ':' + server.port, row[2] + ':' + row[3]);
    });
    assert.deepStrictEqual(
        Array.prototype.map.call(servers.filter(function(server) {
            return server.group === 'official';
        }), function(server) { return server.id; }),
        ['ratspeak-ruby', 'ratspeak-emerald', 'ratspeak-diamond']
    );
    assert.strictEqual(h.context._isPublicTcpServer('rns.ratspeak.org', 4242), true,
        'the Emerald alias stays a public server');
    assert.strictEqual(h.context._isPublicTcpServer('rns.michmesh.net', 4242), false,
        'a preset is its host and its own port');
    assert.strictEqual(h.context._isPublicTcpServer('example.net', 4242), false);

    assert.strictEqual(h.context._isPublicTcpServer('https://2.Ratspeak.org./', 4242), true,
        'a host is matched in the form the Rust limit matches it');

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
    assert(/use ratspeak_runtime::\{[^}]*\bpublic_tcp_server_id\b[^}]*\};/.test(commands) &&
        /use ratspeak_runtime::\{[^}]*\bprojected_enabled_public_tcp_server_ids\b[^}]*\};/.test(commands),
        'the commands match and count with the runtime list');
}

// The sheet's focus trap must wrap at the last control a keyboard can reach.
function testFocusTrapSkipsClosedList() {
    var focused = null;
    function control(name, insideInert) {
        return {
            name: name,
            disabled: false,
            getClientRects: function() { return [{}]; },
            closest: function(selector) {
                assert.strictEqual(selector, '[inert]');
                return insideInert ? {} : null;
            },
            focus: function() { focused = name; }
        };
    }
    var controls = [control('close'), control('toggle'), control('hidden card', true)];
    var keydown = null;
    var modal = {
        querySelectorAll: function() { return controls; },
        addEventListener: function(name, handler) { if (name === 'keydown') keydown = handler; }
    };
    var context = {
        _modalPreviousFocus: null,
        document: { activeElement: controls[1] },
        isMobile: function() { return false; },
        window: {}
    };
    vm.createContext(context);
    vm.runInContext(functionSource(modalsSource, '_trapFocus'), context);
    context._trapFocus(modal);
    assert.strictEqual(focused, 'close');
    var prevented = false;
    keydown({ key: 'Tab', shiftKey: false, preventDefault: function() { prevented = true; } });
    assert(prevented && focused === 'close',
        'Tab on the closed row wraps inside the sheet instead of leaving it');
    context.document.activeElement = controls[0];
    focused = null;
    keydown({ key: 'Tab', shiftKey: true, preventDefault: function() {} });
    assert.strictEqual(focused, 'toggle');
}

function testSources() {
    assert(modalsSource.indexOf("parseInt(document.getElementById('connect-port').value) || 4242") !== -1 &&
        modalsSource.indexOf('quickConnect(server.host, server.port, server.name, { publicServer: true });') !== -1,
        'the preset path carries each server port to the submit');
    assert(modalsSource.indexOf("if (bbCheckbox && opts.publicServer) bbCheckbox.checked = false;") !== -1,
        'a preset always connects as a TCP client, which Backbone servers accept');
    assert.strictEqual(modalsSource.indexOf('UNOFFICIAL'), -1);
    [
        '.peer-details,\n.public-server-more {',
        '.peer-details-toggle,\n.public-server-more-toggle {',
        '.peer-details[data-open="true"] .peer-details-chevron,\n.public-server-more[data-open="true"] .peer-details-chevron { transform: rotate(90deg); }',
        '.peer-details-region,\n.public-server-more-region {',
        '.peer-details[data-open="true"] .peer-details-region,\n.public-server-more[data-open="true"] .public-server-more-region { grid-template-rows: 1fr; }'
    ].forEach(function(rule) {
        assert(viewsCss.indexOf(rule) !== -1, 'the disclosure shares the peer Details rule ' + rule);
    });
    assert(modalsCss.indexOf('.public-server-card--community') !== -1);
    assert(modalsCss.indexOf('.public-server-more-body') !== -1);
}

testGrouping();
testToggle();
testOpensForSavedOrConnected();
testSavedAsBackbone();
testConnect();
testEndpoints();
testFocusTrapSkipsClosedList();
testSources();
console.log('connect public servers tests passed');
