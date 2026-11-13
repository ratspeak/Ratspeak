#!/usr/bin/env node
// The Channels first view: which step it offers for each network and hub
// state, when it re-renders, what the address sheet accepts, and where the
// one-hub sentence appears. Runs the shipped channels.js whole, with the
// shipped stats and interface handlers, against a small DOM model.

'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var jsRoot = path.join(__dirname, '..', '..', 'dashboard', 'static', 'js');
var channelsSource = fs.readFileSync(path.join(jsRoot, 'channels.js'), 'utf8');
var eventsSource = fs.readFileSync(path.join(jsRoot, 'tauri_events.js'), 'utf8');
var navSource = fs.readFileSync(path.join(jsRoot, 'nav.js'), 'utf8');
var indexSource = fs.readFileSync(
    path.join(__dirname, '..', '..', 'dashboard', 'index.html'),
    'utf8'
);

var HUB_A = '7f3a9c03d2e84b7795a0c6e1f3b8d2a4';
var HUB_B = '2b7d41c9e6a05f38914cd2760be3a1f7';
var HUB_C = 'e4c18a5a17c2d09e64b3f8a10c5d09b1';
var LISTEN_WINDOW_MS = 15 * 60 * 1000;

function listenerSource(source, eventName) {
    var start = source.indexOf("RS.listen('" + eventName + "'");
    var end = source.indexOf('\n});', start);
    assert(start !== -1 && end !== -1, eventName + ' listener must exist');
    return source.slice(start, end + 4);
}

function functionSource(source, name) {
    var start = source.indexOf('function ' + name + '(');
    var end = source.indexOf('\n}\n', start);
    assert(start !== -1 && end !== -1, name + ' must exist');
    return source.slice(start, end + 3);
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
    this.className = '';
    this.hidden = false;
    this.disabled = false;
    this.value = '';
    this.innerHTML = '';
    this.isConnected = false;
    this._text = '';
    var element = this;
    this.classList = {
        add: function() {
            Array.prototype.forEach.call(arguments, function(name) {
                if (!element.classList.contains(name)) {
                    element.className = (element.className + ' ' + name).trim();
                }
            });
        },
        remove: function(name) {
            element.className = element.className.split(/\s+/).filter(function(part) {
                return part && part !== name;
            }).join(' ');
        },
        contains: function(name) {
            return element.className.split(/\s+/).indexOf(name) !== -1;
        },
        toggle: function(name, force) {
            var on = force === undefined ? !element.classList.contains(name) : !!force;
            if (on) element.classList.add(name);
            else element.classList.remove(name);
            return on;
        }
    };
}

Object.defineProperty(FakeElement.prototype, 'textContent', {
    get: function() {
        return this._text + this.children.map(function(child) {
            return child.textContent;
        }).join('');
    },
    set: function(value) {
        this.children.forEach(function(child) { child.parentNode = null; });
        this.children = [];
        this._text = String(value == null ? '' : value);
    }
});

Object.defineProperty(FakeElement.prototype, 'firstChild', {
    get: function() { return this.children[0] || null; }
});
Object.defineProperty(FakeElement.prototype, 'childNodes', {
    get: function() { return this.children; }
});

FakeElement.prototype.appendChild = function(child) {
    if (child.parentNode) child.parentNode.removeChild(child);
    child.parentNode = this;
    this.children.push(child);
    return child;
};
FakeElement.prototype.removeChild = function(child) {
    var index = this.children.indexOf(child);
    if (index !== -1) this.children.splice(index, 1);
    child.parentNode = null;
    return child;
};
FakeElement.prototype.setAttribute = function(name, value) {
    this.attributes[name] = String(value);
};
FakeElement.prototype.getAttribute = function(name) {
    return Object.prototype.hasOwnProperty.call(this.attributes, name)
        ? this.attributes[name] : null;
};
FakeElement.prototype.removeAttribute = function(name) {
    delete this.attributes[name];
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
FakeElement.prototype.select = function() {};
FakeElement.prototype.dispatch = function(name) {
    var event = {
        target: this,
        preventDefault: function() {},
        stopPropagation: function() {}
    };
    (this.listeners[name] || []).slice().forEach(function(handler) { handler(event); });
    return event;
};
// A click runs the element's own listeners, then the document's delegates.
FakeElement.prototype.click = function() {
    if (this.disabled) return;
    var event = this.dispatch('click');
    FakeElement.documentClick.forEach(function(handler) { handler(event); });
};
FakeElement.prototype.enter = function(text) {
    this.value = text;
    this.dispatch('input');
};

function matchesSimple(element, selector) {
    var rest = selector;
    var negated = [];
    rest = rest.replace(/:not\(([^)]+)\)/g, function(_all, inner) {
        negated.push(inner);
        return '';
    });
    for (var i = 0; i < negated.length; i++) {
        if (negated[i] === ':disabled') {
            if (element.disabled) return false;
        } else if (matchesSimple(element, negated[i])) {
            return false;
        }
    }
    var tag = rest.match(/^[a-z0-9]+/i);
    if (tag && element.tagName !== tag[0].toUpperCase()) return false;
    var classes = rest.match(/\.[\w-]+/g) || [];
    for (var c = 0; c < classes.length; c++) {
        if (!element.classList.contains(classes[c].slice(1))) return false;
    }
    var attrs = rest.match(/\[[^\]]+\]/g) || [];
    for (var a = 0; a < attrs.length; a++) {
        var parts = attrs[a].slice(1, -1).split('=');
        var name = parts[0];
        var expected = parts.length > 1 ? parts[1].replace(/^["']|["']$/g, '') : null;
        var actual;
        if (name.indexOf('data-') === 0) {
            var key = name.slice(5).replace(/-([a-z])/g, function(_m, ch) {
                return ch.toUpperCase();
            });
            actual = Object.prototype.hasOwnProperty.call(element.dataset, key)
                ? String(element.dataset[key]) : null;
        } else {
            actual = element.getAttribute(name);
        }
        if (actual === null || (expected !== null && actual !== expected)) return false;
    }
    return true;
}

function matches(element, selector) {
    return selector.split(',').some(function(alternative) {
        var chain = alternative.trim().split(/\s+/);
        if (!matchesSimple(element, chain[chain.length - 1])) return false;
        var node = element.parentNode;
        for (var i = chain.length - 2; i >= 0; i--) {
            while (node && !matchesSimple(node, chain[i])) node = node.parentNode;
            if (!node) return false;
            node = node.parentNode;
        }
        return true;
    });
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
FakeElement.prototype.closest = function(selector) {
    for (var node = this; node; node = node.parentNode) {
        if (matches(node, selector)) return node;
    }
    return null;
};

// ---------------------------------------------------------------- harness

function createHarness(options) {
    options = options || {};
    FakeElement.active = null;
    FakeElement.documentClick = [];

    var elements = {};
    [
        'view-channels', 'channels-layout', 'channel-hub-strip', 'channel-hub-summary',
        'channel-hub-menu-btn', 'channel-hub-strip-title', 'channel-hub-strip-meta',
        'channels-list', 'channels-list-label', 'channels-join-btn',
        'channel-room-header', 'channel-compose', 'channel-transcript', 'node-modal'
    ].forEach(function(id) {
        elements[id] = new FakeElement(id === 'channel-hub-summary' ? 'button' : 'div');
        elements[id].isConnected = true;
    });
    elements['view-channels'].className = 'view active';

    var clock = { now: 1700000000000 };
    var timers = [];
    var nextTimer = 1;
    var listeners = {};
    var invocations = [];
    var toasts = [];
    var sheets = [];
    var calls = { openConnectModal: 0, switchView: [] };
    var documentListeners = {};
    var responses = Object.assign({
        discover_channel_hubs: function() { return []; },
        api_saved_channel_hubs: function() { return []; },
        api_channel_room_index: function() { return []; },
        api_channel_unread: function() {
            return { rooms: [], unread_total: 0, mention_total: 0, attention_total: 0 };
        },
        api_blocked_contacts: function() { return []; },
        api_app_settings: function() {
            return {
                public_channel_consent_required_version: 1,
                public_channel_consent_version: 1
            };
        },
        connect_channel_hub: function() { return new Promise(function() {}); }
    }, options.responses || {});

    var context = {
        console: console,
        Promise: Promise,
        _anyInterfaceOnline: null,
        _firstRunHasConfiguredInterface: false,
        lastStats: null,
        _connectionsHasRendered: false,
        _connectionsRenderScheduled: false,
        _connectionsThrottleTimer: null,
        _connectionsFirstLoadTimer: null,
        renderStats: function() {},
        markViewDirty: function() {},
        isViewActive: function(view) {
            return view === 'channels' &&
                elements['view-channels'].classList.contains('active');
        },
        isCompactLayout: function() { return true; },
        activeIdentity: function() { return { display_name: 'Mara' }; },
        localStorage: { getItem: function() { return ''; }, setItem: function() {} },
        navigator: { clipboard: options.clipboard },
        requestAnimationFrame: function(callback) { callback(); },
        setInterval: function() { return 1; },
        clearInterval: function() {},
        setTimeout: function(callback, delay) {
            var timer = { id: nextTimer++, callback: callback, delay: Number(delay) || 0 };
            timers.push(timer);
            return timer.id;
        },
        clearTimeout: function(id) {
            timers = timers.filter(function(timer) { return timer.id !== id; });
        },
        showToast: function(message, tone) { toasts.push({ message: message, tone: tone }); },
        openConnectModal: function() { calls.openConnectModal += 1; },
        switchView: function(view) { calls.switchView.push(view); },
        document: {
            hidden: false,
            get activeElement() { return FakeElement.active; },
            getElementById: function(id) { return elements[id] || null; },
            createElement: function(tag) { return new FakeElement(tag); },
            querySelectorAll: function() { return []; },
            addEventListener: function(name, handler) {
                if (name === 'click') FakeElement.documentClick.push(handler);
                (documentListeners[name] = documentListeners[name] || []).push(handler);
            }
        },
        RS: {
            ui: {
                focusAfterUpdate: function(element) { FakeElement.active = element; }
            },
            qr: { openScanner: function() {} },
            composer: {},
            diag: function() {},
            listen: function(name, handler) {
                (listeners[name] = listeners[name] || []).push(handler);
                return Promise.resolve(function() {});
            },
            invoke: function(name, args) {
                invocations.push({ name: name, args: args });
                if (!responses[name]) return Promise.resolve(null);
                try {
                    return Promise.resolve(responses[name](args));
                } catch (error) {
                    return Promise.reject(error);
                }
            }
        },
        _rsBuildSheet: function(opts, onClose) {
            var overlay = new FakeElement('div');
            var sheet = new FakeElement('div');
            var header = new FakeElement('div');
            var title = new FakeElement('div');
            title.className = 'bottom-sheet-title';
            title.textContent = opts.title;
            header.appendChild(title);
            var body = new FakeElement('div');
            body.className = 'bottom-sheet-body';
            var footer = new FakeElement('div');
            footer.className = 'bottom-sheet-footer';
            sheet.appendChild(header);
            sheet.appendChild(body);
            sheet.appendChild(footer);
            var built = {
                overlay: overlay,
                sheet: sheet,
                body: body,
                footer: footer,
                title: title,
                present: function() {
                    sheet.isConnected = true;
                    sheets.push(built);
                },
                dismiss: function() {
                    if (!sheet.isConnected) return;
                    sheet.isConnected = false;
                    sheets.splice(sheets.indexOf(built), 1);
                    if (onClose) onClose();
                }
            };
            return built;
        }
    };
    context.window = context;
    vm.createContext(context);
    vm.runInContext('Date.now = function() { return __clock.now; };',
        Object.assign(context, { __clock: clock }));
    vm.runInContext(channelsSource, context, { filename: 'channels.js' });
    vm.runInContext(
        functionSource(navSource, '_firstRunInterfaceEnabled') +
            functionSource(navSource, '_firstRunConfiguredInterfaceCount') +
            functionSource(navSource, 'updateFirstRunInterfaceHintGate'),
        context,
        { filename: 'nav-interface-gate.js' }
    );
    vm.runInContext(
        listenerSource(eventsSource, 'stats_update') + '\n' +
            listenerSource(eventsSource, 'hub_interfaces_update'),
        context,
        { filename: 'tauri-events-network.js' }
    );
    (documentListeners.DOMContentLoaded || []).forEach(function(handler) { handler(); });

    function emit(name, payload) {
        (listeners[name] || []).forEach(function(handler) { handler(payload); });
    }

    var harness = {
        context: context,
        elements: elements,
        clock: clock,
        calls: calls,
        toasts: toasts,
        sheets: sheets,
        invocations: invocations,
        emit: emit,
        stats: function(online) {
            emit('stats_update', {
                interface_stats: {
                    interfaces: online === null ? [] : [{ name: 'Ruby', online: online }]
                }
            });
        },
        invoked: function(name) {
            return invocations.filter(function(call) { return call.name === name; });
        },
        timers: function() { return timers.slice(); },
        // Runs timers no longer than maxDelay, including ones they schedule.
        runTimers: function(maxDelay) {
            for (var round = 0; round < 20; round++) {
                var due = timers.filter(function(timer) { return timer.delay <= maxDelay; });
                if (!due.length) return;
                timers = timers.filter(function(timer) { return timer.delay > maxDelay; });
                due.forEach(function(timer) { timer.callback(); });
            }
            throw new Error('timers did not settle');
        },
        settle: async function() {
            for (var i = 0; i < 12; i++) {
                await Promise.resolve();
                harness.runTimers(1000);
            }
        },
        strip: function() {
            return elements['channel-hub-strip-title'].textContent + ' / ' +
                elements['channel-hub-strip-meta'].textContent;
        },
        list: elements['channels-list'],
        pane: elements['channel-transcript'],
        listText: function() { return elements['channels-list'].textContent; },
        rows: function() {
            return elements['channels-list'].querySelectorAll('.channel-hub-row').map(function(row) {
                return row.classList.contains('channel-hub-add-row')
                    ? 'add' : row.dataset.destinationHash;
            });
        },
        sheet: function() { return sheets[sheets.length - 1] || null; }
    };
    return harness;
}

function heardHub(hash, name, hops) {
    return {
        destination_hash: hash,
        identity_hash: 'aa'.repeat(16),
        announced_name: name,
        hops: hops,
        last_seen: 1700000000
    };
}

function savedHub(hash, label) {
    return {
        destination_hash: hash,
        label: label,
        nickname: 'mara',
        added_at: 1699000000,
        last_connected: 1699900000
    };
}

function snapshot(phase, hub, extra) {
    return Object.assign({
        protocol_version: '0.1.3',
        service_model_version: 3,
        generation: 1,
        revision: 1,
        connection_budget: 1,
        selected_hub_destination: hub ? hub.destination_hash : null,
        hubs: [],
        durability: { phase: 'ready', last_error: null },
        history: { phase: 'ready', pending_events: 0, dropped_events: 0, last_error: null },
        phase: phase,
        nickname: hub ? 'mara' : null,
        hub: hub || null,
        rooms: [],
        directory: {
            phase: 'ready', rooms: [], complete: true, omitted_count: 0,
            refreshed_at_ms: 1700000000000, last_error: null
        },
        hub_greeting: null,
        notices: [],
        last_error: null,
        updated_at_ms: 1700000000000
    }, extra || {});
}

var LIVE_HUB = { destination_hash: HUB_A, name: 'Ratspeak Commons', hops: 2 };

function offlineHarness(options) {
    var h = createHarness(options);
    h.context.channelsSnapshot = snapshot('offline', null);
    return h;
}

// ---------------------------------------------------------------- the states

function testNoNetwork() {
    var h = offlineHarness();
    h.stats(false);
    assert.strictEqual(h.strip(), 'No network connection / Channels need a network');
    assert.strictEqual(h.elements['channels-list-label'].textContent, 'Available hubs');
    var empty = h.list.querySelector('.channels-first-empty');
    assert(empty, 'no network must show the explaining block');
    assert.strictEqual(empty.querySelector('.empty-state-primary').textContent,
        'Channels are group chats hosted on hubs');
    assert.strictEqual(empty.querySelector('.empty-state-hint').textContent,
        'Connect to a network and hubs list themselves here.');
    var action = empty.querySelector('.channels-first-action');
    assert.strictEqual(action.textContent, 'Connect to a network');
    assert.deepStrictEqual(h.rows(), [],
        'the address route must not be offered without a network');
    assert.strictEqual(h.pane.querySelector('h3').textContent, 'Connect to a network first');
    assert.strictEqual(h.pane.querySelector('p').textContent,
        'Channels are group chats hosted on hubs. Hubs list themselves once you are online.');
    var paneAction = h.pane.querySelector('button');
    assert.strictEqual(paneAction.textContent, 'Connect to a network');
    assert(paneAction.classList.contains('nr-btn-primary'));
    assert(h.elements['channels-layout'].classList.contains('pane-offers-network'),
        'the list hides its own button only while the pane shows one');

    action.click();
    assert.strictEqual(h.calls.openConnectModal, 1,
        'the button opens the Connect to Network sheet');
    assert.deepStrictEqual(h.calls.switchView, [],
        'the sheet opens over Channels; the view does not change');
    paneAction.click();
    assert.strictEqual(h.calls.openConnectModal, 2);
    assert.strictEqual(h.sheets.length, 0, 'no Channels sheet opens on the way');
}

function testConfiguredButDown() {
    var h = offlineHarness();
    h.stats(false);
    h.emit('hub_interfaces_update', { tcp_client: [{ name: 'Ruby', enabled: 'true' }] });
    var action = h.list.querySelector('.channels-first-action');
    assert.strictEqual(action.textContent, 'Open Network',
        'an interface that is set up but down leads to Network, not to adding another');
    assert.strictEqual(h.pane.querySelector('button').textContent, 'Open Network');
    action.click();
    assert.deepStrictEqual(h.calls.switchView, ['network']);
    assert.strictEqual(h.calls.openConnectModal, 0);
    assert.strictEqual(h.strip(), 'No network connection / Channels need a network');
}

function testListening() {
    var h = offlineHarness();
    h.stats(true);
    assert.strictEqual(h.strip(), 'No hub connected / Network connected');
    var empty = h.list.querySelector('.channels-first-empty');
    assert.strictEqual(empty.querySelector('.empty-state-primary').textContent,
        'Listening for hubs');
    assert.strictEqual(empty.querySelector('.empty-state-hint').textContent,
        'Hubs list themselves here as they announce. Allow up to 15 minutes.');
    assert.strictEqual(empty.querySelector('.channels-first-action'), null);
    assert.deepStrictEqual(h.rows(), ['add']);
    var add = h.list.querySelector('.channel-hub-add-row');
    assert.strictEqual(add.querySelector('.channel-hub-row-title').textContent,
        'Add a hub by address');
    assert.strictEqual(add.querySelector('.channel-hub-row-meta').textContent,
        'For a private or unlisted hub');
    assert.strictEqual(h.list.children[h.list.children.length - 1], add,
        'the address route is the last row');
    assert.strictEqual(h.pane.querySelector('h3').textContent, 'Listening for hubs');
    assert.strictEqual(h.pane.querySelector('p').textContent,
        'Hubs list themselves as they announce. Allow up to 15 minutes.');
    var paneAction = h.pane.querySelector('button');
    assert.strictEqual(paneAction.textContent, 'Add a hub by address');
    assert(paneAction.classList.contains('nr-btn-secondary'));
    assert(!h.elements['channels-layout'].classList.contains('pane-offers-network'));
}

function testStatsNotReceived() {
    var h = offlineHarness();
    h.context.renderChannels();
    assert.strictEqual(h.strip(), 'No hub connected / ',
        'before the first stats the card must not claim a network either way');
    assert.strictEqual(
        h.list.querySelector('.empty-state-primary').textContent, 'Listening for hubs');
    assert.deepStrictEqual(h.rows(), ['add']);
    assert.strictEqual(
        h.elements['channel-hub-summary'].getAttribute('aria-label'),
        'Manage Hub. No hub connected');
}

function testHeard() {
    var h = offlineHarness();
    h.context.channelsDiscoveredHubs = [
        heardHub(HUB_B, 'Ridge Radio Club', 4),
        heardHub(HUB_A, 'Ratspeak Commons', 2)
    ];
    h.stats(true);
    assert.strictEqual(h.strip(), 'No hub connected / Choose a hub below');
    assert.strictEqual(h.elements['channels-list-label'].textContent, 'Available hubs');
    assert.deepStrictEqual(h.rows(), [HUB_A, HUB_B, 'add']);
    assert.strictEqual(h.list.querySelector('.channels-first-empty'), null);
    assert.strictEqual(h.pane.querySelector('h3').textContent, 'Choose a hub');
    assert.strictEqual(h.pane.querySelector('p').textContent,
        'Pick a hub in the list to see its channels.');
    assert.strictEqual(h.pane.querySelector('button'), null);
}

function testSaved() {
    var h = offlineHarness();
    h.context.channelsSavedHubs = [savedHub(HUB_A, 'Ratspeak Commons'), savedHub(HUB_C, 'Field Team')];
    h.context.channelsRoomIndex = [{
        hub_destination_hash: HUB_A, room_name: '#general', topic: 'Say hello',
        latest_recorded_at_ms: 1699900000000, has_history: true
    }];
    h.stats(true);
    assert.strictEqual(h.strip(), 'No hub connected / Choose a saved hub below');
    assert.strictEqual(h.elements['channels-list-label'].textContent, 'Saved hubs');
    assert.deepStrictEqual(h.rows(), [HUB_A, HUB_C, 'add']);
    var sections = h.list.querySelectorAll('.channels-list-section').map(function(section) {
        return section.textContent;
    });
    assert.deepStrictEqual(sections, ['On this device']);
    var order = h.list.children.map(function(child) { return child.className.split(' ')[0]; });
    assert.deepStrictEqual(order, [
        'channel-hub-row', 'channel-hub-row', 'channel-hub-row',
        'channels-list-section', 'channel-room-row'
    ], 'saved hubs, the address row, then history');
    assert.strictEqual(h.pane.querySelector('h3').textContent, 'Choose a hub');

    // Heard and saved together: heard hubs lead under the toolbar label.
    h.context.channelsDiscoveredHubs = [heardHub(HUB_B, 'Ridge Radio Club', 4)];
    h.context.renderChannels();
    assert.strictEqual(h.strip(), 'No hub connected / Choose a hub below');
    assert.strictEqual(h.elements['channels-list-label'].textContent, 'Available hubs');
    assert.deepStrictEqual(h.rows(), [HUB_B, HUB_A, HUB_C, 'add']);
    assert.strictEqual(
        h.list.querySelectorAll('.channels-list-section')[0].textContent, 'Saved hubs');

    // No network with saved hubs: the block explains, the hubs stay, no address row.
    h.context.channelsDiscoveredHubs = [];
    h.stats(false);
    assert.strictEqual(h.strip(), 'No network connection / Channels need a network');
    assert(h.list.querySelector('.channels-first-action'));
    assert.deepStrictEqual(h.rows(), [HUB_A, HUB_C]);
}

function connectedRender(online) {
    var h = createHarness();
    h.context.channelsSnapshot = snapshot('active', LIVE_HUB, {
        rooms: [{ name: '#general', phase: 'joined', members: [], transcript: [] }]
    });
    h.context.channelsDiscoveredHubs = [heardHub(HUB_B, 'Ridge Radio Club', 4)];
    if (online === undefined) h.context.renderChannels();
    else h.stats(online);
    return h;
}

function testConnectedUnchanged() {
    var h = connectedRender(true);
    assert.strictEqual(h.strip(), 'Ratspeak Commons / Connected as mara');
    assert.strictEqual(h.elements['channels-list-label'].textContent, 'Active');
    assert.strictEqual(h.elements['channels-join-btn'].hidden, false);
    assert.strictEqual(h.list.querySelector('.channels-first-empty'), null);
    assert.deepStrictEqual(h.rows(), []);
    assert.deepStrictEqual(
        h.list.querySelectorAll('.channels-list-section').map(function(section) {
            return section.children[0].textContent;
        }),
        ['Discover']
    );
    assert.strictEqual(h.list.querySelectorAll('.channel-room-row').length, 1);
    // A live hub owns the view whatever the interface list says.
    assert.strictEqual(connectedRender(false).listText(), h.listText());
    assert.strictEqual(connectedRender(false).strip(), h.strip());
    assert.strictEqual(connectedRender(undefined).listText(), h.listText());
}

function testConnecting() {
    var h = createHarness();
    h.context.channelsSnapshot = snapshot('resolving', { destination_hash: HUB_A });
    h.stats(true);
    assert.strictEqual(h.listText(), 'Connecting to hub…');
    assert.deepStrictEqual(h.rows(), [], 'no second hub is offered mid-connection');
    assert.strictEqual(h.list.querySelector('.channels-first-empty'), null);

    // An attempt with no hub record yet still reads as an attempt.
    h.context.channelsSnapshot = snapshot('resolving', null, { revision: 2 });
    h.context.renderChannels();
    assert.strictEqual(h.strip(), 'Finding path / ');

    // An ended session keeps its reason on the card.
    h.context.channelsSnapshot = snapshot('error', null, {
        revision: 3, last_error: 'The hub did not answer'
    });
    h.context.renderChannels();
    assert.strictEqual(h.strip(), 'Session ended / The hub did not answer');
    assert.deepStrictEqual(h.rows(), ['add']);
}

// ---------------------------------------------------------------- re-rendering

async function testRendersOnNetworkChange() {
    var h = offlineHarness({
        responses: {
            discover_channel_hubs: function() { return [heardHub(HUB_A, 'Ratspeak Commons', 2)]; }
        }
    });
    h.stats(false);
    assert.strictEqual(h.strip(), 'No network connection / Channels need a network');
    var firstBlock = h.list.children[0];
    h.stats(false);
    assert.strictEqual(h.list.children[0], firstBlock,
        'an unchanged network state must not re-render the view');
    assert.strictEqual(h.invoked('discover_channel_hubs').length, 0);

    h.stats(true);
    assert.strictEqual(h.strip(), 'No hub connected / Network connected',
        'the view moves on in the same stats event that brings the network up');
    assert.strictEqual(h.invoked('discover_channel_hubs').length, 1,
        'coming online reads the announce cache once');
    await h.settle();
    assert.strictEqual(h.strip(), 'No hub connected / Choose a hub below');
    assert.deepStrictEqual(h.rows(), [HUB_A, 'add']);

    h.stats(true);
    assert.strictEqual(h.invoked('discover_channel_hubs').length, 1);
    h.stats(false);
    assert.strictEqual(h.strip(), 'No network connection / Channels need a network',
        'losing the network returns the view to the first step');
}

async function testRendersOnHeardHub() {
    var heard = [];
    var h = offlineHarness({
        responses: { discover_channel_hubs: function() { return heard; } }
    });
    h.stats(true);
    await h.settle();
    assert.deepStrictEqual(h.rows(), ['add']);
    var reads = h.invoked('discover_channel_hubs').length;

    heard = [heardHub(HUB_B, 'Ridge Radio Club', 4)];
    h.emit('announce_received', {});
    h.emit('announce_received', {});
    assert.deepStrictEqual(h.rows(), ['add'], 'announces are coalesced before the read');
    await h.settle();
    assert.strictEqual(h.invoked('discover_channel_hubs').length, reads + 1);
    assert.deepStrictEqual(h.rows(), [HUB_B, 'add']);
    assert.strictEqual(h.strip(), 'No hub connected / Choose a hub below');
}

function testListeningWindow() {
    var h = offlineHarness();
    h.stats(true);
    h.context.renderChannels();
    h.context.renderChannels();
    var waiting = h.timers().filter(function(timer) { return timer.delay > 1000; });
    assert.strictEqual(waiting.length, 1, 'one timer waits for the window; nothing polls');
    assert.strictEqual(waiting[0].delay, LISTEN_WINDOW_MS);
    assert(h.list.querySelector('.channels-first-empty-icon').classList.contains('conn-loading-pulse'));

    h.clock.now += LISTEN_WINDOW_MS;
    h.runTimers(LISTEN_WINDOW_MS);
    assert.strictEqual(h.list.querySelector('.empty-state-primary').textContent,
        'Listening for hubs');
    assert.strictEqual(h.list.querySelector('.empty-state-hint').textContent,
        'No hub has announced on this network yet.');
    assert(!h.list.querySelector('.channels-first-empty-icon').classList.contains('conn-loading-pulse'));
    assert.strictEqual(h.pane.querySelector('p').textContent,
        'No hub has announced on this network yet.');
    assert.deepStrictEqual(h.rows(), ['add']);
    assert.strictEqual(h.timers().length, 0);

    // The window restarts with the network.
    h.stats(false);
    h.stats(true);
    assert.strictEqual(h.list.querySelector('.empty-state-hint').textContent,
        'Hubs list themselves here as they announce. Allow up to 15 minutes.');
}

// ---------------------------------------------------------------- address sheet

function openAddress(h) {
    h.context._channelsApplyPublicConsentSettings({
        public_channel_consent_required_version: 1,
        public_channel_consent_version: 1
    });
    h.list.querySelector('.channel-hub-add-row').click();
    return h.settle().then(function() {
        var built = h.sheet();
        assert(built, 'the address row opens a sheet');
        return {
            built: built,
            input: built.sheet.querySelector('input.mono'),
            connect: built.footer.querySelector('.nr-btn-primary'),
            error: built.body.querySelector('.channel-sheet-error'),
            paste: built.body.querySelector('.channel-sheet-field-label-row button'),
            scan: built.body.querySelector('.channel-link-method')
        };
    });
}

async function testAddressSheet() {
    var h = offlineHarness({
        clipboard: { readText: function() { return Promise.resolve('  ' + HUB_A + '\n'); } },
        responses: {
            api_preview_channel_share: function(args) {
                if (args.payload.indexOf('hub=' + HUB_B) === -1) {
                    throw new Error('Not a valid Ratspeak channel share target');
                }
                return {
                    format: 'ratspeak.channel.v1',
                    payload: args.payload,
                    hub_destination_hash: HUB_B,
                    room: args.payload.indexOf('room=') === -1 ? null : '#general'
                };
            },
            connect_channel_hub: function() { throw new Error('No path to this hub'); }
        }
    });
    h.stats(true);
    var s = await openAddress(h);
    assert.strictEqual(s.built.title.textContent, 'Add a hub by address');
    assert.strictEqual(s.built.body.children[0].textContent,
        'Ask the hub’s host for its link, QR code, or address.');
    assert.strictEqual(s.input.placeholder, 'ratspeak:// link or 32 characters');
    assert.strictEqual(s.built.body.querySelector('label').textContent, 'Hub address');
    assert.strictEqual(s.paste.textContent, 'Paste');
    assert.strictEqual(s.scan.querySelector('strong').textContent, 'Scan QR code');
    assert.strictEqual(s.built.body.querySelector('.channel-connection-identity strong').textContent,
        'Mara');
    assert.strictEqual(s.connect.textContent, 'Connect');
    assert.strictEqual(s.connect.disabled, true, 'nothing to connect to yet');
    assert.strictEqual(FakeElement.active, s.input, 'focus enters the sheet');

    // Anything that is neither a link nor 32 hexadecimal characters is refused locally.
    var refused = [
        'hello', HUB_A.slice(1), HUB_A + '0', 'https://ratspeak.org/hub', HUB_A.replace('7', 'g')
    ];
    for (var i = 0; i < refused.length; i++) {
        s.input.enter(refused[i]);
        assert.strictEqual(s.error.textContent, '');
        assert.strictEqual(s.connect.disabled, false);
        s.connect.click();
        await h.settle();
        assert.strictEqual(s.error.textContent,
            'Enter a ratspeak:// link or a 32-character hub address.', refused[i]);
    }
    assert.strictEqual(h.invoked('api_preview_channel_share').length, 0);
    assert.strictEqual(h.invoked('connect_channel_hub').length, 0);
    assert.strictEqual(s.connect.textContent, 'Connect');

    // Typing clears the message.
    s.input.enter('');
    assert.strictEqual(s.error.textContent, '');
    assert.strictEqual(s.connect.disabled, true);

    // A link the native parser rejects.
    s.input.enter('ratspeak://channel?v=1&hub=nope');
    s.connect.click();
    await h.settle();
    assert.strictEqual(h.invoked('api_preview_channel_share').length, 1);
    assert.strictEqual(s.error.textContent, 'Not a valid Ratspeak channel share target');
    assert.strictEqual(h.invoked('connect_channel_hub').length, 0);

    // A 32-character address connects without the link parser, in lower case.
    s.input.enter(' ' + HUB_A.toUpperCase() + ' ');
    s.connect.click();
    await h.settle();
    assert.strictEqual(h.invoked('api_preview_channel_share').length, 1);
    assert.deepStrictEqual(
        JSON.parse(JSON.stringify(h.invoked('connect_channel_hub')[0].args)),
        { args: { destination_hash: HUB_A, nickname: 'Mara' } }
    );
    assert.strictEqual(s.error.textContent, 'No path to this hub',
        'a failed connection stays on the sheet with its reason');
    assert.strictEqual(s.connect.textContent, 'Connect');
    assert.strictEqual(s.built.sheet.isConnected, true);

    // A hub link is read by the native parser, then connects to its hub.
    s.input.enter('ratspeak://channel?v=1&hub=' + HUB_B);
    s.connect.click();
    await h.settle();
    assert.strictEqual(h.invoked('api_preview_channel_share')[1].args.payload,
        'ratspeak://channel?v=1&hub=' + HUB_B);
    assert.strictEqual(h.invoked('connect_channel_hub')[1].args.args.destination_hash, HUB_B);
    assert.strictEqual(h.context.channelsPendingShareJoin, null);

    // Paste fills the field from the clipboard, trimmed.
    s.paste.click();
    await h.settle();
    assert.strictEqual(s.input.value, HUB_A);
    assert.strictEqual(s.connect.disabled, false);
    assert.strictEqual(h.toasts.length, 0);
}

async function testAddressSheetRoomLink() {
    var h = offlineHarness({
        responses: {
            api_preview_channel_share: function(args) {
                return {
                    format: 'ratspeak.channel.v1', payload: args.payload,
                    hub_destination_hash: HUB_B, room: '#general'
                };
            }
        }
    });
    h.stats(true);
    var s = await openAddress(h);
    s.input.enter('ratspeak://channel?v=1&hub=' + HUB_B + '&room=%23general');
    s.connect.click();
    await h.settle();
    assert.strictEqual(h.invoked('connect_channel_hub').length, 1);
    assert.deepStrictEqual(JSON.parse(JSON.stringify(h.context.channelsPendingShareJoin)), {
        destination_hash: HUB_B, room: '#general', generation: 1
    }, 'a channel link joins its channel once the hub welcomes the session');
    assert.strictEqual(s.connect.textContent, 'Connecting…');
}

async function testPasteUnavailable() {
    var denied = offlineHarness({
        clipboard: { readText: function() { return Promise.reject(new Error('denied')); } }
    });
    denied.stats(true);
    var s = await openAddress(denied);
    s.input.enter('kept');
    s.paste.click();
    await denied.settle();
    assert.strictEqual(s.error.textContent,
        'Could not read the clipboard. Type or paste into the field.');
    assert.strictEqual(denied.toasts.length, 0, 'the sheet says it, not a toast under its scrim');
    assert.strictEqual(s.input.value, 'kept');
    s.input.enter('kept more');
    assert.strictEqual(s.error.textContent, '', 'typing clears the message');
    assert.strictEqual(s.built.sheet.isConnected, true, 'the sheet stays usable');

    var throwing = offlineHarness({
        clipboard: { readText: function() { throw new Error('blocked'); } }
    });
    throwing.stats(true);
    s = await openAddress(throwing);
    s.paste.click();
    await throwing.settle();
    assert.strictEqual(s.error.textContent,
        'Could not read the clipboard. Type or paste into the field.');
    assert.strictEqual(s.built.sheet.isConnected, true);

    var missing = offlineHarness({ clipboard: undefined });
    missing.stats(true);
    s = await openAddress(missing);
    assert.strictEqual(s.paste.hidden, true,
        'a platform without clipboard read offers no Paste control');
    assert.strictEqual(s.input.hidden, false);
}

// ---------------------------------------------------------------- switching

async function testSwitchConfirmation() {
    var h = connectedRender(true);
    h.context._channelsApplyPublicConsentSettings({
        public_channel_consent_required_version: 1,
        public_channel_consent_version: 1
    });
    h.context.channelsOpenConnectSheet({ destination_hash: HUB_B, announced_name: 'Ridge Radio Club' });
    await h.settle();
    var review = h.sheet();
    assert.strictEqual(review.title.textContent, 'Switch to Ridge Radio Club?');
    var impact = review.body.querySelector('.channel-hub-switch-impact');
    assert.strictEqual(impact.hidden, false);
    assert.strictEqual(impact.textContent,
        'One hub can be live at a time. Live channels on Ratspeak Commons end; ' +
        'history stays on this device.');
    assert.strictEqual(review.footer.querySelector('.nr-btn-primary').textContent, 'Switch');
    review.dismiss();

    // The chooser itself carries no warning and names its list for what it holds.
    h.context.channelsOpenHubSwitcher();
    await h.settle();
    var switcher = h.sheet();
    assert.strictEqual(switcher.title.textContent, 'Switch hub');
    assert.strictEqual(switcher.sheet.textContent.indexOf('One hub can be live'), -1);
    assert.deepStrictEqual(
        switcher.body.querySelectorAll('.channels-list-section').map(function(section) {
            return section.children[0].textContent;
        }),
        ['Current', 'Available']
    );
    var add = switcher.footer.querySelectorAll('button').filter(function(button) {
        return button.textContent === 'Add by address';
    })[0];
    assert(add && !add.hidden && add.classList.contains('nr-btn-secondary'));

    // From a live hub the address sheet hands over to the confirmation.
    add.click();
    await h.settle();
    var address = h.sheet();
    assert.strictEqual(address.title.textContent, 'Add a hub by address');
    var primary = address.footer.querySelector('.nr-btn-primary');
    assert.strictEqual(primary.textContent, 'Review');
    address.sheet.querySelector('input.mono').enter(HUB_C);
    primary.click();
    await h.settle();
    assert.strictEqual(h.invoked('connect_channel_hub').length, 0,
        'a live hub is never replaced without its confirmation');
    assert.strictEqual(
        h.sheet().body.querySelector('.channel-hub-switch-impact').textContent.indexOf(
            'One hub can be live at a time.'), 0);
}

async function testFirstUseCarriesNoWarning() {
    var h = offlineHarness();
    h.context._channelsApplyPublicConsentSettings({
        public_channel_consent_required_version: 1,
        public_channel_consent_version: 1
    });
    h.context.channelsDiscoveredHubs = [heardHub(HUB_A, 'Ratspeak Commons', 2)];
    h.stats(true);
    var firstView = h.strip() + h.listText() + h.pane.textContent;
    assert.strictEqual(firstView.indexOf('One hub'), -1);
    assert.strictEqual(firstView.indexOf('Connect to a hub'), -1);

    h.list.querySelector('.channel-hub-row').click();
    await h.settle();
    var review = h.sheet();
    assert.strictEqual(review.title.textContent, 'Connect to Ratspeak Commons?');
    assert.strictEqual(review.body.querySelector('.channel-hub-switch-impact').hidden, true);
    assert.strictEqual(review.sheet.textContent.indexOf('One hub'), -1);
    assert.strictEqual(review.footer.querySelector('.nr-btn-primary').textContent, 'Connect');
    review.dismiss();

    // The chooser sheet, reached before any hub is live, is as lean.
    h.context.channelsDiscoveredHubs = [];
    h.context.channelsOpenHubSwitcher();
    await h.settle();
    var switcher = h.sheet();
    assert.strictEqual(switcher.sheet.textContent.indexOf('One hub'), -1);
    assert.strictEqual(switcher.body.querySelector('.channel-active-empty').textContent,
        'No hubs heard yet');
    switcher.dismiss();

    h.stats(false);
    h.context.channelsOpenHubSwitcher();
    await h.settle();
    var offlineAdd = h.sheet().footer.querySelectorAll('button').filter(function(button) {
        return button.textContent === 'Add by address';
    })[0];
    assert.strictEqual(offlineAdd.hidden, true,
        'the address route is not offered without a network');
}

function testSources() {
    var removed = [
        'Choose a hub to begin', 'No channel hubs yet', 'No other hubs heard recently',
        'Saved hubs remain available below', 'Enter a trusted hub address to begin',
        'Connect to a trusted hub, then choose a channel.', 'Connect to a hub',
        "'Nearby'", 'Find a hub'
    ];
    removed.forEach(function(text) {
        assert.strictEqual(channelsSource.indexOf(text), -1, text + ' must be gone from channels.js');
        assert.strictEqual(indexSource.indexOf(text), -1, text + ' must be gone from index.html');
    });
    assert.strictEqual(channelsSource.split('One hub can be live at a time.').length, 2,
        'the one-hub sentence has one home');
}

async function main() {
    testNoNetwork();
    testConfiguredButDown();
    testListening();
    testStatsNotReceived();
    testHeard();
    testSaved();
    testConnectedUnchanged();
    testConnecting();
    await testRendersOnNetworkChange();
    await testRendersOnHeardHub();
    testListeningWindow();
    await testAddressSheet();
    await testAddressSheetRoomLink();
    await testPasteUnavailable();
    await testSwitchConfirmation();
    await testFirstUseCarriesNoWarning();
    testSources();
    console.log('channels first view tests passed');
}

main().catch(function(error) {
    console.error(error);
    process.exit(1);
});
