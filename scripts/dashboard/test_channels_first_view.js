#!/usr/bin/env node
// The Channels first view: which step it offers for each network and hub
// state, when it re-renders, what the address sheet accepts, where the
// one-hub sentence appears, what Reconnect does, and what a tap does without
// a network. Runs the shipped channels.js and channel_hub.js whole, with the
// shipped stats and interface handlers, against a small DOM model.

'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var jsRoot = path.join(__dirname, '..', '..', 'dashboard', 'static', 'js');
var chatScrollSource = fs.readFileSync(path.join(jsRoot, 'chat_scroll.js'), 'utf8');
var channelsSource = fs.readFileSync(path.join(jsRoot, 'channels.js'), 'utf8');
var channelHubSource = fs.readFileSync(path.join(jsRoot, 'channel_hub.js'), 'utf8');
var kitSource = fs.readFileSync(path.join(jsRoot, 'ui_kit.js'), 'utf8');
var eventsSource = fs.readFileSync(path.join(jsRoot, 'tauri_events.js'), 'utf8');
var navSource = fs.readFileSync(path.join(jsRoot, 'nav.js'), 'utf8');
var indexSource = fs.readFileSync(
    path.join(__dirname, '..', '..', 'dashboard', 'index.html'),
    'utf8'
);

var HUB_A = '7f3a9c03d2e84b7795a0c6e1f3b8d2a4';
var HUB_B = '2b7d41c9e6a05f38914cd2760be3a1f7';
var HUB_C = 'e4c18a5a17c2d09e64b3f8a10c5d09b1';
var OWN_HUB = '51d0c7a2be9f4f1083a6d5e2c4b7a990';
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
        'view-channels', 'channels-layout', 'channel-hub-strip', 'channels-list',
        'channel-room-header', 'channel-compose', 'channel-transcript', 'node-modal'
    ].forEach(function(id) {
        elements[id] = new FakeElement('div');
        elements[id].isConnected = true;
    });
    // Rows the views build carry ids of their own, such as the hub strip's row.
    function findById(id) {
        if (elements[id]) return elements[id];
        var found = null;
        Object.keys(elements).some(function(key) {
            (function walk(node) {
                node.children.forEach(function(child) {
                    if (found) return;
                    if (child.id === id) found = child;
                    else walk(child);
                });
            })(elements[key]);
            return !!found;
        });
        return found;
    }
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
            getElementById: findById,
            createElement: function(tag) { return new FakeElement(tag); },
            createTextNode: function(text) { return { textContent: String(text) }; },
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
    context.addEventListener = function() {};
    vm.createContext(context);
    vm.runInContext('Date.now = function() { return __clock.now; };',
        Object.assign(context, { __clock: clock }));
    vm.runInContext(kitSource, context, { filename: 'ui_kit.js' });
    vm.runInContext(chatScrollSource, context, { filename: 'chat_scroll.js' });
    vm.runInContext(channelsSource, context, { filename: 'channels.js' });
    vm.runInContext(channelHubSource, context, { filename: 'channel_hub.js' });
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
            var meta = findById('channel-hub-strip-meta');
            return findById('channel-hub-strip-title').textContent + ' / ' + (meta ? meta.textContent : '');
        },
        stripRow: function() { return findById('channel-hub-summary'); },
        list: elements['channels-list'],
        pane: elements['channel-transcript'],
        listText: function() { return elements['channels-list'].textContent; },
        captions: function() {
            return elements['channels-list'].querySelectorAll('.ui-caption').map(function(caption) {
                return caption.textContent;
            });
        },
        // The first step a phone shows in the list; desktop explains it in the room pane.
        phoneEmpty: function() {
            return elements['channels-list'].querySelector('.ui-phone-only .ui-empty');
        },
        rows: function() {
            return elements['channels-list'].querySelectorAll('.ch-hub-row').map(function(row) {
                return row.classList.contains('ch-add-row')
                    ? 'add' : row.dataset.destinationHash;
            });
        },
        paneTitle: function() {
            var title = elements['channel-transcript'].querySelector('.ui-empty-title');
            return title ? title.textContent : null;
        },
        paneBody: function() {
            var body = elements['channel-transcript'].querySelector('.ui-empty-body');
            return body ? body.textContent : null;
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
    assert.strictEqual(h.strip(), 'Not connected / No network');
    assert.strictEqual(h.stripRow().tagName, 'DIV', 'with no hub the strip says why; it is not a menu');
    assert.strictEqual(h.stripRow().getAttribute('role'), 'status');
    var empty = h.phoneEmpty();
    assert(empty, 'a phone explains no network in the list');
    assert.strictEqual(empty.querySelector('.ui-empty-title').textContent, 'Channels need a network');
    assert.strictEqual(empty.querySelector('.ui-empty-body').textContent,
        'Channels are group chats hosted on hubs. Connect to a network to find hubs nearby and on the internet.');
    var action = empty.querySelector('button');
    assert.strictEqual(action.textContent, 'Connect to a network');
    assert(action.classList.contains('ui-btn--primary'));
    assert.deepStrictEqual(h.captions(), ['Hubs'], 'desktop keeps the list a list');
    assert(h.listText().indexOf('Hubs you can join show here once you’re online.') !== -1);
    assert.deepStrictEqual(h.rows(), [],
        'the address route is not offered without a network');
    assert.strictEqual(h.paneTitle(), 'Channels need a network');
    var paneAction = h.pane.querySelector('button');
    assert.strictEqual(paneAction.textContent, 'Connect to a network');
    assert(paneAction.classList.contains('ui-btn--primary'));

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
    var action = h.phoneEmpty().querySelector('button');
    assert.strictEqual(action.textContent, 'Open Network',
        'an interface that is set up but down leads to Network, not to adding another');
    assert.strictEqual(h.pane.querySelector('button').textContent, 'Open Network');
    action.click();
    assert.deepStrictEqual(h.calls.switchView, ['network']);
    assert.strictEqual(h.calls.openConnectModal, 0);
    assert.strictEqual(h.strip(), 'Not connected / No network');
}

function testListening() {
    var h = offlineHarness();
    h.stats(true);
    assert.strictEqual(h.strip(), 'No hub yet / Network connected');
    var empty = h.phoneEmpty();
    assert.strictEqual(empty.querySelector('.ui-empty-title').textContent, 'Listening for hubs');
    assert.strictEqual(empty.querySelector('.ui-empty-body').textContent,
        'Hubs show up as they announce. Most announce every 15 minutes, so this can take a little while.');
    assert.strictEqual(empty.querySelector('button').textContent, 'Add a hub by address');
    assert(!empty.querySelector('button').classList.contains('ui-btn--primary'),
        'adding by address is the Secondary route while listening');
    assert.deepStrictEqual(h.captions(), ['Hubs nearby']);
    assert(h.list.querySelector('.ch-wait .ui-spin'), 'desktop lists a quiet listening line');
    assert.deepStrictEqual(h.rows(), []);
    assert.strictEqual(h.paneTitle(), 'Listening for hubs');
    assert.strictEqual(h.pane.querySelector('button').textContent, 'Add a hub by address');
    assert.strictEqual(h.pane.querySelector('button').dataset.channelAction, 'add-address');
}

function testStatsNotReceived() {
    var h = offlineHarness();
    h.context.renderChannels();
    assert.strictEqual(h.strip(), 'No hub yet / ',
        'before the first stats the strip must not claim a network either way');
    assert.strictEqual(h.phoneEmpty().querySelector('.ui-empty-title').textContent, 'Listening for hubs');
    assert.strictEqual(h.stripRow().getAttribute('aria-label'), 'No hub yet');
}

function testHeard() {
    var h = offlineHarness();
    h.context.channelsDiscoveredHubs = [
        heardHub(HUB_B, 'Ridge Radio Club', 4),
        heardHub(HUB_A, 'Ratspeak Commons', 2)
    ];
    h.stats(true);
    assert.strictEqual(h.strip(), 'No hub yet / Network connected');
    assert.deepStrictEqual(h.captions(), ['Hubs nearby']);
    assert.deepStrictEqual(h.rows(), [HUB_A, HUB_B, 'add']);
    var row = h.list.querySelector('.ch-hub-row');
    assert.strictEqual(row.querySelector('.ui-row-sub .t').textContent, '2 hops away',
        'a hub row says how far it is, in words');
    assert.strictEqual(h.phoneEmpty(), null);
    assert.strictEqual(h.paneTitle(), 'Choose a hub');
    assert.strictEqual(h.paneBody(), 'A hub hosts channels. Pick one from the list, or add one by its address.');
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
    assert.deepStrictEqual(h.rows(), [HUB_A, HUB_C, 'add']);
    assert.deepStrictEqual(h.captions(), ['Saved', 'On this device']);
    assert.strictEqual(h.list.querySelector('.ch-hub-row .ui-row-sub .t').textContent, 'Saved hub');
    var history = h.list.querySelectorAll('.ch-room-row');
    assert.strictEqual(history.length, 1, 'retained history stays reachable without a hub');
    assert.strictEqual(history[0].querySelector('.ui-row-sub .t').textContent, 'Say hello');
    assert.strictEqual(h.paneTitle(), 'Choose a hub');

    // Heard and saved together: heard hubs lead, each group under its caption.
    h.context.channelsDiscoveredHubs = [heardHub(HUB_B, 'Ridge Radio Club', 4)];
    h.context.renderChannels();
    assert.deepStrictEqual(h.rows(), [HUB_B, HUB_A, HUB_C, 'add']);
    assert.deepStrictEqual(h.captions(), ['Hubs nearby', 'Saved', 'On this device']);

    // No network: the list explains; hubs and the address row wait for the network.
    h.context.channelsDiscoveredHubs = [];
    h.stats(false);
    assert.strictEqual(h.strip(), 'Not connected / No network');
    assert(h.phoneEmpty().querySelector('.ui-btn--primary'));
    assert.deepStrictEqual(h.rows(), []);
    assert.deepStrictEqual(h.captions(), ['Hubs', 'On this device']);
}

function connectedRender(online, options) {
    var h = createHarness(options);
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
    assert.strictEqual(h.stripRow().tagName, 'BUTTON');
    assert.strictEqual(h.stripRow().getAttribute('aria-haspopup'), 'menu');
    assert.strictEqual(h.stripRow().getAttribute('aria-label'),
        'Ratspeak Commons, Connected as mara. Switch hub or see hub options');
    assert.strictEqual(h.phoneEmpty(), null);
    assert.deepStrictEqual(h.rows(), []);
    assert.deepStrictEqual(h.captions(), ['Joined', 'Discover on this hub']);
    assert.strictEqual(h.list.querySelectorAll('.ch-room-row').length, 1);
    // A live hub owns the view whatever the interface list says.
    assert.strictEqual(connectedRender(false).listText(), h.listText());
    assert.strictEqual(connectedRender(false).strip(), h.strip());
    assert.strictEqual(connectedRender(undefined).listText(), h.listText());
}

function testConnecting() {
    var h = createHarness();
    h.context.channelsSnapshot = snapshot('resolving', { destination_hash: HUB_A, name: 'Ratspeak Commons' }, {
        hubs: [{ destination_hash: HUB_A, durable: { rooms: [{ name: '#general' }, { name: '#help' }] } }]
    });
    h.stats(true);
    assert.strictEqual(h.strip(), 'Ratspeak Commons / Connecting…');
    assert.deepStrictEqual(h.captions(), ['Joined']);
    var rejoining = h.list.querySelectorAll('.ch-room-row');
    assert.strictEqual(rejoining.length, 2, 'joined rooms wait for the hub');
    assert.strictEqual(rejoining[0].querySelector('.ui-row-sub .t').textContent,
        'Rejoins when the hub answers');
    assert.deepStrictEqual(h.rows(), [], 'no second hub is offered mid-connection');
    assert.strictEqual(h.paneTitle(), 'Connecting to Ratspeak Commons');
    assert.strictEqual(h.paneBody(),
        'Securing the link. Your channels rejoin on their own once the hub answers.');
    assert.strictEqual(h.pane.querySelector('button').textContent, 'Cancel');

    // An attempt with no rooms to rejoin explains itself in the list on a phone.
    h.context.channelsSnapshot = snapshot('resolving', { destination_hash: HUB_A, name: 'Ratspeak Commons' }, { revision: 2 });
    h.context.renderChannels();
    assert.strictEqual(h.phoneEmpty().querySelector('.ui-empty-title').textContent,
        'Connecting to Ratspeak Commons');

    // An ended session says so on the strip and keeps the hub's menu.
    h.context.channelsSnapshot = snapshot('error', LIVE_HUB, {
        revision: 3, last_error: 'The hub did not answer'
    });
    h.context.renderChannels();
    assert.strictEqual(h.strip(), 'Ratspeak Commons / Session ended');
    assert.strictEqual(h.paneBody(), 'The hub did not answer');
}

// ---------------------------------------------------------------- re-rendering

async function testRendersOnNetworkChange() {
    var h = offlineHarness({
        responses: {
            discover_channel_hubs: function() { return [heardHub(HUB_A, 'Ratspeak Commons', 2)]; }
        }
    });
    h.stats(false);
    assert.strictEqual(h.strip(), 'Not connected / No network');
    var firstBlock = h.list.children[0];
    h.stats(false);
    assert.strictEqual(h.list.children[0], firstBlock,
        'an unchanged network state must not re-render the view');
    assert.strictEqual(h.invoked('discover_channel_hubs').length, 0);

    h.stats(true);
    assert.strictEqual(h.strip(), 'No hub yet / Network connected',
        'the view moves on in the same stats event that brings the network up');
    assert.strictEqual(h.invoked('discover_channel_hubs').length, 1,
        'coming online reads the announce cache once');
    await h.settle();
    assert.deepStrictEqual(h.rows(), [HUB_A, 'add']);

    h.stats(true);
    assert.strictEqual(h.invoked('discover_channel_hubs').length, 1);
    h.stats(false);
    assert.strictEqual(h.strip(), 'Not connected / No network',
        'losing the network returns the view to the first step');
}

async function testRendersOnHeardHub() {
    var heard = [];
    var h = offlineHarness({
        responses: { discover_channel_hubs: function() { return heard; } }
    });
    h.stats(true);
    await h.settle();
    assert.deepStrictEqual(h.rows(), []);
    var reads = h.invoked('discover_channel_hubs').length;

    heard = [heardHub(HUB_B, 'Ridge Radio Club', 4)];
    h.emit('announce_received', {});
    h.emit('announce_received', {});
    assert.deepStrictEqual(h.rows(), [], 'announces are coalesced before the read');
    await h.settle();
    assert.strictEqual(h.invoked('discover_channel_hubs').length, reads + 1);
    assert.deepStrictEqual(h.rows(), [HUB_B, 'add']);
    assert.strictEqual(h.strip(), 'No hub yet / Network connected');
}

function testListeningWindow() {
    var h = offlineHarness();
    h.stats(true);
    h.context.renderChannels();
    h.context.renderChannels();
    var waiting = h.timers().filter(function(timer) { return timer.delay > 1000; });
    assert.strictEqual(waiting.length, 1, 'one timer waits for the window; nothing polls');
    assert.strictEqual(waiting[0].delay, LISTEN_WINDOW_MS);
    assert.strictEqual(h.list.querySelector('.ch-wait').textContent, 'Listening for hubs');

    h.clock.now += LISTEN_WINDOW_MS;
    h.runTimers(LISTEN_WINDOW_MS);
    assert.strictEqual(h.phoneEmpty().querySelector('.ui-empty-title').textContent,
        'Listening for hubs');
    assert.strictEqual(h.phoneEmpty().querySelector('.ui-empty-body').textContent,
        'No hubs heard yet.');
    assert.strictEqual(h.list.querySelector('.ch-wait').textContent, 'No hubs heard yet');
    assert.strictEqual(h.paneBody(), 'No hubs heard yet.');
    assert.strictEqual(h.timers().length, 0);

    // The window restarts with the network.
    h.stats(false);
    h.stats(true);
    assert.strictEqual(h.phoneEmpty().querySelector('.ui-empty-body').textContent,
        'Hubs show up as they announce. Most announce every 15 minutes, so this can take a little while.');
}

// ---------------------------------------------------------------- address sheet

function openAddress(h) {
    h.context._channelsApplyPublicConsentSettings({
        public_channel_consent_required_version: 1,
        public_channel_consent_version: 1
    });
    var add = h.list.querySelector('.ch-add-row') ||
        h.pane.querySelector('[data-channel-action="add-address"]');
    add.click();
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

// The hub strip's menu; the harness is a phone, so it is a sheet.
async function openMenu(h) {
    h.stripRow().click();
    await h.settle();
    var built = h.sheet();
    assert.strictEqual(built.title.textContent, 'Hubs');
    return built;
}

function menuItems(built) {
    return built.body.querySelectorAll('.ui-menu-item');
}

function menuItem(built, label) {
    return menuItems(built).filter(function(item) {
        return item.querySelector('.lbl').children[0].textContent === label;
    })[0] || null;
}

async function testSwitchConfirmation() {
    var h = connectedRender(true, {
        responses: {
            discover_channel_hubs: function() { return [heardHub(HUB_B, 'Ridge Radio Club', 4)]; }
        }
    });
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
        'Only one hub can be connected at a time. You will leave Ratspeak Commons. ' +
        'History stays on this device.');
    assert.strictEqual(review.footer.querySelector('.nr-btn-primary').textContent, 'Switch');
    review.dismiss();

    // The hub menu itself carries no warning: the hubs, then add, share, details, and disconnect.
    var menu = await openMenu(h);
    assert.strictEqual(menu.sheet.textContent.indexOf('Only one hub'), -1);
    assert.deepStrictEqual(menuItems(menu).map(function(item) {
        return item.querySelector('.lbl').children[0].textContent;
    }), ['Ratspeak Commons', 'Ridge Radio Club', 'Add a hub by address…', 'Share this hub',
        'Hub details', 'Disconnect']);
    assert.strictEqual(menuItem(menu, 'Ratspeak Commons').getAttribute('aria-checked'), 'true');

    // Choosing another hub hands over to the confirmation.
    menuItem(menu, 'Ridge Radio Club').click();
    await h.settle();
    assert.strictEqual(h.sheet().title.textContent, 'Switch to Ridge Radio Club?');
    h.sheet().dismiss();

    // From a live hub the address sheet hands over to the confirmation too.
    menu = await openMenu(h);
    menuItem(menu, 'Add a hub by address…').click();
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
            'Only one hub can be connected at a time.'), 0);
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
    assert.strictEqual(firstView.indexOf('one hub'), -1);
    assert.strictEqual(firstView.indexOf('Connect to a hub'), -1);

    h.list.querySelector('.ch-hub-row').click();
    await h.settle();
    var review = h.sheet();
    assert.strictEqual(review.title.textContent, 'Connect to Ratspeak Commons?');
    assert.strictEqual(review.body.querySelector('.channel-hub-switch-impact').hidden, true);
    assert.strictEqual(review.sheet.textContent.indexOf('one hub'), -1);
    assert.strictEqual(review.footer.querySelector('.nr-btn-primary').textContent, 'Connect');
    review.dismiss();
}

// ---------------------------------------------------------------- reconnect

var NETWORK_FIRST = { message: 'Connect to a network first.', tone: 'toast-warning' };

function consented(h) {
    h.context._channelsApplyPublicConsentSettings({
        public_channel_consent_required_version: 1,
        public_channel_consent_version: 1
    });
}

function hubState(hash, nickname) {
    return {
        destination_hash: hash,
        desired: { connected: true, nickname: nickname, rooms: [] },
        observed: null,
        durable: { saved: false },
        recovery: {}
    };
}

// The runtime clears the session nickname when a session ends.
function endedSnapshot(hub, extra) {
    return snapshot('error', hub, Object.assign({
        nickname: null,
        last_error: 'Channel link closed'
    }, extra || {}));
}

function connectArgs(h, index) {
    return JSON.parse(JSON.stringify(h.invoked('connect_channel_hub')[index || 0].args));
}

async function testReconnect() {
    var h = createHarness({
        responses: {
            connect_channel_hub: function(args) {
                return snapshot('resolving', { destination_hash: args.args.destination_hash }, {
                    revision: 2, nickname: args.args.nickname
                });
            }
        }
    });
    consented(h);
    h.context.channelsSavedHubs = [savedHub(HUB_A, 'Ratspeak Commons')];
    h.context.channelsSnapshot = endedSnapshot(LIVE_HUB, { hubs: [hubState(HUB_A, 'fieldrat')] });
    h.stats(true);
    assert.strictEqual(h.paneTitle(), 'The channel session ended');
    assert.strictEqual(h.paneBody(), 'Channel link closed');
    var reconnect = h.pane.querySelector('button');
    assert.strictEqual(reconnect.textContent, 'Reconnect');

    reconnect.click();
    assert.strictEqual(h.sheets.length, 0, 'Reconnect opens neither the chooser nor a review');
    assert.deepStrictEqual(connectArgs(h), {
        args: { destination_hash: HUB_A, nickname: 'fieldrat' }
    }, 'the ended session’s hub and nickname are used again');
    assert.strictEqual(h.context.channelsPendingHubLabel, 'Ratspeak Commons');
    reconnect.click();
    assert.strictEqual(h.invoked('connect_channel_hub').length, 1,
        'a second press does not start a second attempt');
    await h.settle();
    assert.strictEqual(h.paneTitle(), 'Connecting to Ratspeak Commons',
        'the pane goes straight to the connecting state');
    assert.strictEqual(h.pane.querySelector('button').textContent, 'Cancel');
    assert.strictEqual(h.sheets.length, 0);
    assert.strictEqual(h.toasts.length, 0);

    function press(ended, saved) {
        var harness = createHarness();
        consented(harness);
        harness.context.channelsSavedHubs = saved || [];
        harness.context.channelsSnapshot = ended;
        harness.stats(true);
        harness.pane.querySelector('button').click();
        return harness;
    }

    // Without a session nickname the saved hub's is used; without either, the identity's.
    assert.deepStrictEqual(
        connectArgs(press(endedSnapshot(LIVE_HUB), [savedHub(HUB_A, 'Ratspeak Commons')])),
        { args: { destination_hash: HUB_A, nickname: 'mara' } });
    assert.deepStrictEqual(connectArgs(press(endedSnapshot(LIVE_HUB))),
        { args: { destination_hash: HUB_A, nickname: 'Mara' } });

    // The selected hub is enough when the hub record is gone.
    assert.deepStrictEqual(
        connectArgs(press(endedSnapshot(null, {
            selected_hub_destination: HUB_B, hubs: [hubState(HUB_B, 'ridge')]
        }))),
        { args: { destination_hash: HUB_B, nickname: 'ridge' } });

    // With no hub named anywhere, adding one by address is the way back.
    var unknown = press(endedSnapshot(null, { selected_hub_destination: null }));
    assert.strictEqual(unknown.invoked('connect_channel_hub').length, 0);
    await unknown.settle();
    assert.strictEqual(unknown.sheet().title.textContent, 'Add a hub by address');

    // A refused command says why and leaves the button usable.
    var refused = createHarness({
        responses: {
            connect_channel_hub: function() { throw new Error('Channels are unavailable'); }
        }
    });
    consented(refused);
    refused.context.channelsSnapshot = endedSnapshot(LIVE_HUB);
    refused.stats(true);
    var again = refused.pane.querySelector('button');
    again.click();
    await refused.settle();
    assert.deepStrictEqual(refused.toasts,
        [{ message: 'Channels are unavailable', tone: 'toast-error' }]);
    assert.strictEqual(again.disabled, false);
    assert.strictEqual(refused.sheets.length, 0);
    assert.strictEqual(refused.paneTitle(), 'The channel session ended');
}

// ---------------------------------------------------------------- no network

function ownHubOverview(running) {
    return {
        supported: true,
        created: true,
        destination_hash: OWN_HUB,
        settings: { enabled: true, hub_name: 'Mara’s Den' },
        status: {
            running: running, destination_hash: OWN_HUB,
            welcomed_sessions: 0, registered_rooms: 1
        }
    };
}

async function testNoNetworkTaps() {
    var h = offlineHarness({
        responses: {
            discover_channel_hubs: function() { return [heardHub(HUB_B, 'Ridge Radio Club', 4)]; }
        }
    });
    consented(h);
    h.context.channelsSavedHubs = [savedHub(HUB_A, 'Ratspeak Commons')];
    h.context.channelsDiscoveredHubs = [heardHub(HUB_B, 'Ridge Radio Club', 4)];
    h.stats(false);
    assert.deepStrictEqual(h.rows(), [], 'without a network the list explains instead of offering hubs');

    // A review asked for anyway says what is missing instead of opening.
    h.context.channelsOpenConnectSheet(heardHub(HUB_B, 'Ridge Radio Club', 4));
    await h.settle();
    assert.strictEqual(h.sheets.length, 0, 'no review opens for a hub that cannot be reached');
    assert.deepStrictEqual(h.toasts, [NETWORK_FIRST]);
    assert.strictEqual(h.invoked('connect_channel_hub').length, 0);

    // In the hub menu other hubs and the address route wait for the network; no toast under the scrim.
    var live = connectedRender(false, {
        responses: {
            discover_channel_hubs: function() { return [heardHub(HUB_B, 'Ridge Radio Club', 4)]; }
        }
    });
    consented(live);
    var menu = await openMenu(live);
    assert.strictEqual(menuItem(menu, 'Ridge Radio Club').disabled, true,
        'a hub that cannot be reached is not offered');
    assert.strictEqual(menuItem(menu, 'Add a hub by address…').disabled, true);
    assert.strictEqual(menuItem(menu, 'Disconnect').disabled, false);
    assert.strictEqual(live.toasts.length, 0);

    // The open menu follows the network.
    live.stats(true);
    assert.strictEqual(menuItem(menu, 'Ridge Radio Club').disabled, false);
    assert.strictEqual(menuItem(menu, 'Add a hub by address…').disabled, false);
    live.stats(false);
    assert.strictEqual(menuItem(menu, 'Ridge Radio Club').disabled, true);

    // With a network the same item leads to the review.
    live.stats(true);
    menuItem(menu, 'Ridge Radio Club').click();
    await live.settle();
    assert.strictEqual(live.sheet().title.textContent, 'Switch to Ridge Radio Club?');
    assert.strictEqual(live.toasts.length, 0);

    // Reconnect does not start an attempt that cannot succeed.
    var ended = createHarness();
    consented(ended);
    ended.context.channelsSnapshot = endedSnapshot(LIVE_HUB, { hubs: [hubState(HUB_A, 'fieldrat')] });
    ended.stats(false);
    var reconnect = ended.pane.querySelector('button');
    assert.strictEqual(reconnect.textContent, 'Reconnect');
    reconnect.click();
    await ended.settle();
    assert.strictEqual(ended.invoked('connect_channel_hub').length, 0);
    assert.strictEqual(ended.sheets.length, 0);
    assert.deepStrictEqual(ended.toasts, [NETWORK_FIRST]);
    assert.strictEqual(reconnect.disabled, false);

    // Before the first stats nothing is known to be missing, so nothing is held back.
    var unknown = offlineHarness({
        responses: {
            discover_channel_hubs: function() { return [heardHub(HUB_B, 'Ridge Radio Club', 4)]; }
        }
    });
    consented(unknown);
    unknown.context.channelsDiscoveredHubs = [heardHub(HUB_B, 'Ridge Radio Club', 4)];
    unknown.context.renderChannels();
    unknown.list.querySelector('.ch-hub-row').click();
    await unknown.settle();
    assert.strictEqual(unknown.sheet().title.textContent, 'Connect to Ridge Radio Club?');
    assert.strictEqual(unknown.toasts.length, 0);
}

async function testOwnHubNeedsNoNetwork() {
    var overview = ownHubOverview(true);
    function ownHarness() {
        var harness = createHarness({
            responses: { api_channel_hub: function() { return overview; } }
        });
        consented(harness);
        return harness;
    }

    // Reconnecting to this device's own hub is never held back.
    var h = ownHarness();
    await h.context.channelHubLoad(true);
    h.context.channelsSnapshot = endedSnapshot(
        { destination_hash: OWN_HUB, name: 'Mara’s Den' },
        { hubs: [hubState(OWN_HUB, 'mara')] }
    );
    h.stats(false);
    h.pane.querySelector('button').click();
    assert.deepStrictEqual(connectArgs(h), {
        args: { destination_hash: OWN_HUB, nickname: 'mara' }
    });
    assert.strictEqual(h.toasts.length, 0);

    // Its review opens too; another hub's does not.
    var review = ownHarness();
    await review.context.channelHubLoad(true);
    review.context.channelsSnapshot = snapshot('offline', null);
    review.stats(false);
    review.context.channelsOpenConnectSheet({ destination_hash: HUB_B });
    await review.settle();
    assert.strictEqual(review.sheets.length, 0);
    assert.deepStrictEqual(review.toasts, [NETWORK_FIRST]);
    review.context.channelsOpenConnectSheet({ destination_hash: OWN_HUB });
    await review.settle();
    assert.strictEqual(review.sheet().title.textContent.indexOf('Connect to '), 0);
    assert.strictEqual(review.toasts.length, 1);
}

async function testOwnHubRunningNoNetwork() {
    var overview = ownHubOverview(true);
    var h = offlineHarness({
        responses: { api_channel_hub: function() { return overview; } }
    });
    consented(h);
    h.stats(false);
    assert.strictEqual(h.strip(), 'Not connected / No network');

    await h.context.channelHubLoad(true);
    assert.strictEqual(h.strip(), 'No hub yet / Your hub is running',
        'the view follows the hub as it starts');
    var empty = h.phoneEmpty();
    assert.strictEqual(empty.querySelector('.ui-empty-title').textContent, 'Your hub is running');
    assert.strictEqual(empty.querySelector('.ui-empty-body').textContent,
        'Other hubs need a network connection.');
    var buttons = empty.querySelectorAll('button');
    assert.strictEqual(buttons.length, 2);
    var open = buttons[0];
    assert.strictEqual(open.textContent, 'Open hub');
    assert.strictEqual(open.dataset.channelAction, 'open-owned-hub');
    assert(open.classList.contains('ui-btn--primary'));
    var network = buttons[1];
    assert.strictEqual(network.textContent, 'Connect to a network');
    assert(!network.classList.contains('ui-btn--primary'),
        'the network action is not the primary one here');
    assert.strictEqual(h.paneTitle(), 'Your hub is running');
    assert.strictEqual(h.pane.querySelector('button').textContent, 'Open hub');

    // The view returns to the network step when the hub stops, and back when it starts.
    h.emit('channel_hub_snapshot', { running: false, destination_hash: OWN_HUB });
    assert.strictEqual(h.strip(), 'Not connected / No network');
    assert.strictEqual(h.phoneEmpty().querySelector('.ui-empty-title').textContent,
        'Channels need a network');
    assert.strictEqual(h.phoneEmpty().querySelectorAll('button').length, 1);
    assert.strictEqual(h.paneTitle(), 'Channels need a network');
    h.emit('channel_hub_snapshot', { running: true, destination_hash: OWN_HUB });
    assert.strictEqual(h.strip(), 'No hub yet / Your hub is running');

    network = h.phoneEmpty().querySelectorAll('button')[1];
    network.click();
    assert.strictEqual(h.calls.openConnectModal, 1);
    h.phoneEmpty().querySelector('.ui-btn--primary').click();
    await h.settle();
    assert.deepStrictEqual(connectArgs(h), {
        args: { destination_hash: OWN_HUB, nickname: 'Mara' }
    }, 'the hub on this device opens without a network');
    assert.strictEqual(h.toasts.length, 0);

    // With a network the room pane offers the hub; the list keeps listening.
    var online = offlineHarness({
        responses: { api_channel_hub: function() { return overview; } }
    });
    await online.context.channelHubLoad(true);
    online.stats(true);
    assert.strictEqual(online.strip(), 'No hub yet / Network connected');
    assert.strictEqual(online.paneTitle(), 'Open your hub');
}

// ---------------------------------------------------------------- wording

async function testConsentWording() {
    var h = offlineHarness({
        responses: {
            api_app_settings: function() {
                return {
                    public_channel_consent_required_version: 1,
                    public_channel_consent_version: 0
                };
            }
        }
    });
    await h.settle();
    h.context.channelsDiscoveredHubs = [heardHub(HUB_A, 'Ratspeak Commons', 2)];
    h.stats(true);
    h.list.querySelector('.ch-hub-row').click();
    await h.settle();
    var sheet = h.sheet();
    assert.strictEqual(sheet.title.textContent, 'Public channels');
    var fact = sheet.body.querySelector('.channel-consent-fact-copy');
    assert.strictEqual(fact.children[0].textContent, 'Independent operators');
    assert.strictEqual(fact.children[1].textContent,
        'Each hub is run and moderated by its operator, not Ratspeak. ' +
        'Ratspeak may not be able to remove its content.');
    assert(!/official/i.test(sheet.sheet.textContent), 'nothing marks a hub official');
}

function testSources() {
    var removed = [
        'Choose a hub to begin', 'No channel hubs yet', 'No other hubs heard recently',
        'Saved hubs remain available below', 'Enter a trusted hub address to begin',
        'Connect to a trusted hub, then choose a channel.', 'Connect to a hub',
        'Find a hub', 'list themselves', 'Allow up to 15 minutes',
        'No hub has announced', 'One hub can be live', 'Unless marked official',
        'The link contains no channel key'
    ];
    removed.forEach(function(text) {
        assert.strictEqual(channelsSource.indexOf(text), -1, text + ' must be gone from channels.js');
        assert.strictEqual(indexSource.indexOf(text), -1, text + ' must be gone from index.html');
    });
    var channelsMarkup = indexSource.slice(
        indexSource.indexOf('id="view-channels"'),
        indexSource.indexOf('<div class="view"', indexSource.indexOf('id="view-channels"'))
    );
    assert(channelsMarkup.indexOf('id="channel-transcript"') !== -1);
    // Owner, 2026-11-14: hub rows say how far a hub is ("Direct, nearby", "2 hops away").
    assert(channelsSource.indexOf("_channelsListGroup(list, 'Hubs nearby')") !== -1);
    assert(channelsSource.indexOf("'Direct, nearby'") !== -1);
    assert.strictEqual(channelsSource.split('Most announce every 15 minutes').length, 2,
        'the listening copy has one home');
    assert.strictEqual(channelsSource.split('Only one hub can be connected at a time.').length, 2,
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
    await testReconnect();
    await testNoNetworkTaps();
    await testOwnHubNeedsNoNetwork();
    await testOwnHubRunningNoNetwork();
    await testConsentWording();
    testSources();
    console.log('channels first view tests passed');
}

main().catch(function(error) {
    console.error(error);
    process.exit(1);
});
