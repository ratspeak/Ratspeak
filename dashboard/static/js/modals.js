var _modalPreviousFocus = null;

// The Network screen lists these: 'official' up front, 'community' under Unofficial.
var PUBLIC_TCP_SERVERS = [
    { id: 'ratspeak-ruby', group: 'official', name: 'Ruby', host: '1.ratspeak.org', port: 4141, tone: 'ruby' },
    { id: 'ratspeak-emerald', group: 'official', name: 'Emerald', host: '2.ratspeak.org', port: 4242, tone: 'emerald', aliases: [{ host: 'rns.ratspeak.org', port: 4242 }] },
    { id: 'ratspeak-diamond', group: 'official', name: 'Diamond', host: '3.ratspeak.org', port: 4343, tone: 'diamond' },
    { id: 'apollo-rns', group: 'community', name: 'Apollo RNS', host: 'rns.not-a-number.io', port: 4242, place: 'Belgium' },
    { id: 'beleth', group: 'community', name: 'Beleth', host: 'rns.beleth.net', port: 4242 },
    { id: 'michmesh', group: 'community', name: 'MichMesh', host: 'rns.michmesh.net', port: 7822, place: 'Michigan, US' },
    { id: 'one-big-network', group: 'community', name: 'One-Big-Network', host: 'rns.one-big.network', port: 4242, place: 'Germany' },
    { id: 'rmap', group: 'community', name: 'RMAP', host: 'rmap.world', port: 4242, place: 'Italy' },
    { id: 'swiss-rns', group: 'community', name: 'Swiss RNS', host: 'swiss-rns.cl0ud.ch', port: 4242, place: 'Switzerland' },
    { id: 'utn-oregon', group: 'community', name: 'UTN Oregon', host: 'rns.utn.lol', port: 4965, place: 'Oregon, US' },
    { id: 'washmesh', group: 'community', name: 'Washmesh', host: 'reticulum.washmesh.net', port: 7242, place: 'Washington, US' },
];

// The host is brought to the form the Rust transport limit matches on.
function _tcpServerKey(host, port) {
    var value = String(host || '').trim().toLowerCase();
    var scheme = value.indexOf('://');
    if (scheme >= 0) value = value.slice(scheme + 3);
    value = value.split('/')[0].replace(/\.+$/, '');
    return value + ':' + (parseInt(port, 10) || 4242);
}

function _publicServerEndpointKeys(server) {
    var endpoints = [{ host: server.host, port: server.port }].concat(server.aliases || []);
    return endpoints.map(function(endpoint) {
        return _tcpServerKey(endpoint.host, endpoint.port);
    });
}

// The public server a Join on the Network screen is waiting for.
var _connectPendingPublicServerKey = null;

function _publicServerMatchesEndpoint(server, host, port) {
    var endpointKey = _tcpServerKey(host, port);
    return _publicServerEndpointKeys(server).indexOf(endpointKey) >= 0;
}

function _trapFocus(modalEl) {
    _modalPreviousFocus = document.activeElement;
    function focusableControls() {
        return Array.prototype.filter.call(modalEl.querySelectorAll(
            'button, [href], input:not([type="hidden"]), select, textarea, [tabindex]:not([tabindex="-1"])'
        ), function(control) {
            // An inert region keeps its layout boxes, so the rect test alone would count it.
            return !control.disabled && control.getClientRects().length > 0 && !control.closest('[inert]');
        });
    }
    var focusable = focusableControls();
    var wantsKeyboardFocus = window.RS && RS.ui && typeof RS.ui.prefersKeyboardFocus === 'function'
        ? RS.ui.prefersKeyboardFocus()
        : !isMobile();
    if (focusable.length > 0 && wantsKeyboardFocus) {
        var first = focusable[0];
        if (!isMobile() || (first.tagName !== 'INPUT' && first.tagName !== 'TEXTAREA' && first.tagName !== 'SELECT')) {
            first.focus();
        }
    }

    modalEl._focusTrapHandler = function(e) {
        if (e.key !== 'Tab') return;
        var items = focusableControls();
        if (items.length === 0) return;
        var first = items[0];
        var last = items[items.length - 1];
        if (e.shiftKey && document.activeElement === first) {
            e.preventDefault();
            last.focus();
        } else if (!e.shiftKey && document.activeElement === last) {
            e.preventDefault();
            first.focus();
        }
    };
    modalEl.addEventListener('keydown', modalEl._focusTrapHandler);
}

function _releaseFocus(modalEl) {
    if (modalEl._focusTrapHandler) {
        modalEl.removeEventListener('keydown', modalEl._focusTrapHandler);
        delete modalEl._focusTrapHandler;
    }
    var restoreFocus = window.RS && RS.ui && typeof RS.ui.prefersKeyboardFocus === 'function'
        ? RS.ui.prefersKeyboardFocus()
        : !isMobile();
    if (restoreFocus && _modalPreviousFocus && _modalPreviousFocus.focus) {
        _modalPreviousFocus.focus();
    } else if (modalEl.contains(document.activeElement) && document.activeElement.blur) {
        document.activeElement.blur();
    }
    _modalPreviousFocus = null;
}

var currentModalNode = null;

function openNodeModal(nodeId) {
    currentModalNode = nodeId;

    document.getElementById('modal-title').textContent = friendlyNode(nodeId);
    document.getElementById('modal-name').value = nodeNames[nodeId] || 'My Hub';
    document.getElementById('modal-hash').textContent = '\u2014';

    loadHubInterfaces();

    RS.ui.openExistingSheet('node-modal', 'node-modal-overlay');
}

function closeNodeModal() {
    RS.ui.closeExistingSheet('node-modal', 'node-modal-overlay');
    currentModalNode = null;
}

document.getElementById('modal-close').addEventListener('click', closeNodeModal);

document.getElementById('modal-name').addEventListener('input', function() {
    if (!currentModalNode) return;
    var val = this.value.trim();

    if (val && val !== 'My Hub') {
        nodeNames[currentModalNode] = val;
    } else {
        delete nodeNames[currentModalNode];
    }

    document.getElementById('modal-title').textContent = friendlyNode(currentModalNode);

});

function applyModalTransportModePayload(data) {
    if (RS.ui && typeof RS.ui.applyTransportModePayload === 'function') {
        RS.ui.applyTransportModePayload('modal-transport-select', data);
    }
}

var _modalTransportBadge = document.getElementById('modal-transport-select');
if (_modalTransportBadge) {
    function _openModalTransportChoice() {
        if (RS.ui && typeof RS.ui.openTransportModeChoice === 'function') {
            RS.ui.openTransportModeChoice(_modalTransportBadge);
        }
    }

    if (RS.ui && typeof RS.ui.bindTransportChoice === 'function') {
        RS.ui.bindTransportChoice(_modalTransportBadge);
    } else {
        _modalTransportBadge.addEventListener('click', _openModalTransportChoice);
        _modalTransportBadge.addEventListener('keydown', function(e) {
            if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); _openModalTransportChoice(); }
        });
    }
}

RS.listen('transport_mode_updated', function(data) {
    applyModalTransportModePayload(data);
});

function loadHubInterfaces() {
    var sections = [
        'modal-lora-list',
        'modal-auto-list',
        'modal-tcp-client-list',
        'modal-tcp-server-list',
        'modal-backbone-client-list',
        'modal-backbone-server-list',
    ];
    sections.forEach(function(id) {
        var el = document.getElementById(id);
        if (el) el.innerHTML = '<span class="inline-hint">Loading...</span>';
    });

    RS.invoke('api_hub_interfaces').then(function(ifaces) {
        window._hubInterfacesData = ifaces || null;
        if (ifaces && ifaces.transport) applyModalTransportModePayload(ifaces.transport);
        renderModalInterfaceSection('modal-lora-list', ifaces.rnode || [], 'rnode');
        renderModalInterfaceSection('modal-auto-list', ifaces.auto || [], 'auto');
        renderModalInterfaceSection('modal-tcp-client-list', ifaces.tcp_client || [], 'tcp_client');
        renderModalInterfaceSection('modal-tcp-server-list', ifaces.tcp_server || [], 'tcp_server');
        renderModalInterfaceSection('modal-backbone-client-list', ifaces.backbone_client || [], 'backbone_client');
        renderModalInterfaceSection('modal-backbone-server-list', ifaces.backbone_server || [], 'backbone_server');
    }).catch(function() {
        sections.forEach(function(id) {
            var el = document.getElementById(id);
            if (el) el.innerHTML = '<span class="inline-hint">Error loading interfaces.</span>';
        });
    });
}

function _statsNameMatchesConfig(statsName, configName) {
    var m = statsName.match(/\w+\[([^\/\]]+)/);
    if (m) return m[1].trim() === configName;
    return statsName.indexOf(configName) !== -1;
}

function getInterfaceLiveStatus(ifaceName) {
    if (!lastStats || !lastStats.interface_stats) return null;
    var ifaces = lastStats.interface_stats.interfaces || [];
    for (var i = 0; i < ifaces.length; i++) {
        if (_statsNameMatchesConfig(ifaces[i].name, ifaceName)) {
            return ifaces[i];
        }
    }
    return null;
}

function _configuredInterfacePaused(ifaceName) {
    var data = window._hubInterfacesData || null;
    var groups = ['rnode', 'auto', 'tcp_client', 'tcp_server', 'backbone_client', 'backbone_server'];
    for (var g = 0; g < groups.length; g++) {
        var entries = (data && data[groups[g]]) || [];
        for (var i = 0; i < entries.length; i++) {
            var entry = entries[i] || {};
            if (entry.name !== ifaceName) continue;
            var enabled = entry.enabled;
            if (enabled === undefined || enabled === null) enabled = entry.interface_enabled;
            if (enabled === undefined || enabled === null) return false;
            return /^(false|no|0|off)$/i.test(String(enabled).trim());
        }
    }
    return false;
}

function updateHubModalStatusDots() {
    document.querySelectorAll('#node-modal .hub-iface-status').forEach(function(dot) {
        var ifaceName = dot.dataset.ifaceName;
        if (!ifaceName) return;
        var liveData = getInterfaceLiveStatus(ifaceName);
        dot.className = 'hub-iface-status';
        if (_configuredInterfacePaused(ifaceName)) {
            dot.classList.add('paused');
            dot.title = 'Paused';
        } else if (liveData) {
            dot.classList.add(liveData.online ? 'up' : 'down');
            dot.title = liveData.online ? 'Connected' : 'Disconnected';
        } else {
            dot.classList.add('unknown');
            dot.title = 'Waiting for status...';
        }
    });
}

function renderModalInterfaceSection(containerId, interfaces, ifaceType) {
    var container = document.getElementById(containerId);
    if (!container) return;

    if (interfaces.length === 0) {
        var emptyMsg = {
            'rnode': 'No LoRa interfaces configured.',
            'auto': 'Not enabled.',
            'tcp_client': 'No connections configured.',
            'tcp_server': 'No servers configured.',
            'backbone_client': 'No Backbone connections configured.',
            'backbone_server': 'No Backbone servers configured.',
        };
        container.innerHTML = '<span class="inline-hint">' + (emptyMsg[ifaceType] || 'None.') + '</span>';
        return;
    }

    container.innerHTML = '';
    interfaces.forEach(function(iface) {
        if (RS.ui && typeof RS.ui.createInterfaceRow === 'function') {
            container.appendChild(RS.ui.createInterfaceRow(iface, ifaceType));
        }
    });
}

function getIfaceDetailText(iface, ifaceType) {
    if (ifaceType === 'rnode') {
        var freqMhz = iface.frequency ? _rnodeFormatMhz(iface.frequency) : '?';
        var bwKhz = iface.bandwidth ? ((parseInt(iface.bandwidth, 10) || 0) / 1000).toFixed(0) : '?';
        var portInfo = iface.port || '?';
        if (typeof portInfo === 'string' && portInfo.indexOf('ble://') === 0) {
            var bleTarget = portInfo.substring(6);
            portInfo = 'BLE: ' + (bleTarget || 'auto');
        } else if (_rnodeIsTcpPort(portInfo)) {
            portInfo = 'TCP: ' + _rnodeTcpInputValue(portInfo);
        }
        return freqMhz + ' MHz | BW ' + bwKhz + 'k | SF' + (iface.spreadingfactor || '?') +
            ' | CR' + (iface.codingrate || '?') + ' | ' + portInfo;
    }
    if (ifaceType === 'auto') {
        return 'WiFi / LAN auto-discovery';
    }
    if (ifaceType === 'tcp_client') {
        return (iface.target_host || '?') + ':' + (iface.target_port || '?');
    }
    if (ifaceType === 'tcp_server') {
        return 'Listening on :' + (iface.listen_port || '?');
    }
    if (ifaceType === 'backbone_client') {
        return (iface.target_host || '?') + ':' + (iface.target_port || '?');
    }
    if (ifaceType === 'backbone_server') {
        return 'Listening on :' + (iface.listen_port || '?');
    }
    return '';
}

function removeHubInterface(ifaceType, ifaceName) {
    var eventMap = {
        'rnode': 'remove_lora_interface',
        'auto': 'disable_auto_interface',
        'tcp_client': 'remove_tcp_connection',
        'tcp_server': 'remove_tcp_server',
        'backbone_client': 'remove_backbone_connection',
        'backbone_server': 'remove_backbone_server',
        'ble_peer': 'disable_ble_peer_interface',
    };
    var event = eventMap[ifaceType];
    if (!event) return;

    var statusEl = document.getElementById('modal-op-status');
    if (statusEl) {
        statusEl.style.display = 'block';
        statusEl.textContent = 'Removing...';
        statusEl.className = 'modal-op-status active';
    }

    document.querySelectorAll('#node-modal .danger-btn-sm').forEach(function(btn) {
        btn.disabled = true;
    });

    // disable_ble_peer_interface takes no args (singleton); rest take { name }.
    var invokeArgs = event === 'disable_ble_peer_interface' ? {} : { name: ifaceName };
    RS.invoke(event, invokeArgs).catch(function(err) {
        var message = (err && err.message) || 'Failed to update interface';
        if (statusEl) {
            statusEl.textContent = message;
            statusEl.className = 'modal-op-status error';
        }
        document.querySelectorAll('#node-modal .danger-btn-sm').forEach(function(btn) {
            btn.disabled = false;
        });
        showToast(message, 'toast-error', 8000);
    });
}

// ------------------------------------------------------------------ connection dialogs
// Add your own server, Host setup, Set up a radio, and Local Network options: one kit Dialog
// each (a Sheet on a phone), Medium, with a Quiet Cancel on desktop and one Primary.

function _netdlgEsc(value) {
    return escapeHtml(value == null ? '' : String(value));
}

function _netdlgIcon(name) {
    return RS.kit.icon(name);
}

// A kit text field with its help and an error slot. Numbers are text with an inputmode, never a
// number input (#85: WebKitGTK draws oversized spin buttons).
// o: { type, value, placeholder, mode, max, unit, mono, help, aria, attrs }.
function _netdlgTextField(id, label, o) {
    o = o || {};
    var input = '<input class="ui-input' + (o.mono ? ' ui-input--mono' : '') + '" id="' + id + '" type="' + (o.type || 'text') + '"' +
        (o.mode ? ' inputmode="' + o.mode + '"' : '') + (o.max ? ' maxlength="' + o.max + '"' : '') +
        ' value="' + _netdlgEsc(o.value) + '" placeholder="' + _netdlgEsc(o.placeholder) + '"' +
        (o.help ? ' aria-describedby="' + id + '-help"' : '') + (label ? '' : ' aria-label="' + _netdlgEsc(o.aria) + '"') +
        ' autocomplete="off" autocorrect="off" autocapitalize="none" spellcheck="false">';
    if (o.unit) input = '<div class="ui-input-wrap">' + input + '<span class="ui-unit">' + o.unit + '</span></div>';
    return '<div class="ui-field"' + (o.attrs || '') + '>' + (label ? '<label class="ui-label" for="' + id + '">' + label + '</label>' : '') + input +
        (o.help ? '<p class="ui-help" id="' + id + '-help">' + o.help + '</p>' : '') +
        '<p class="ui-error" id="' + id + '-error" role="alert" hidden></p></div>';
}

function _netdlgSwitch(id, label, desc, on, o) {
    o = o || {};
    return '<div class="ui-prop"' + (o.attrs || '') + '><span class="ui-prop-text"><span class="ui-prop-label" id="' + id + '-label">' + label + '</span>' +
        (desc ? '<span class="ui-prop-desc" id="' + id + '-desc">' + desc + '</span>' : '') + '</span>' +
        '<label class="ui-switch"><input type="checkbox" role="switch" id="' + id + '"' + (on ? ' checked' : '') + (o.disabled ? ' disabled' : '') +
        ' aria-labelledby="' + id + '-label"' + (desc ? ' aria-describedby="' + id + '-desc"' : '') + '><span class="track"></span></label></div>';
}

function _netdlgSeg(name, items, current, label, disabled) {
    return '<div class="ui-seg ui-seg--block" role="radiogroup" aria-label="' + label + '" data-netdlg-seg="' + name + '">' + items.map(function(item) {
        return '<button type="button" role="radio" aria-checked="' + (item[0] === current) + '" data-v="' + item[0] + '"' + (disabled ? ' disabled' : '') + '>' +
            _netdlgEsc(item[1]) + '</button>';
    }).join('') + '</div>';
}

function _netdlgSegSet(root, name, value) {
    root.querySelectorAll('[data-netdlg-seg="' + name + '"] [role="radio"]').forEach(function(btn) {
        btn.setAttribute('aria-checked', btn.getAttribute('data-v') === value ? 'true' : 'false');
    });
}

// A chevron row that shows or hides the stacked row after it (id + '-body').
function _netdlgDisclosure(id, label, desc, open) {
    return '<button type="button" class="ui-prop" id="' + id + '" data-netdlg="disclose" aria-expanded="' + !!open + '" aria-controls="' + id + '-body">' +
        '<span class="ui-prop-text"><span class="ui-prop-label">' + label + '</span><span class="ui-prop-desc">' + desc + '</span></span>' +
        _netdlgIcon(open ? 'chevron-down' : 'chevron-right') + '</button>';
}

function _netdlgSetDisclosure(id, open) {
    var btn = document.getElementById(id);
    var body = document.getElementById(id + '-body');
    if (btn) {
        btn.setAttribute('aria-expanded', open ? 'true' : 'false');
        var chevron = btn.querySelector('.ui-ico');
        if (chevron) chevron.outerHTML = _netdlgIcon(open ? 'chevron-down' : 'chevron-right');
    }
    if (body) body.hidden = !open;
}

// Kit Select fields: a button that opens a menu. Each keeps its value and choices here, so a
// catalog loaded before any dialog opens still fills them.
var _netdlgSelects = {};

function _netdlgSelect(id) {
    return _netdlgSelects[id] || (_netdlgSelects[id] = { value: '', options: [], onPick: null });
}

function _netdlgSelectGet(id) {
    return _netdlgSelect(id).value;
}

function _netdlgSelectLabel(id) {
    var select = _netdlgSelect(id);
    var match = select.options.filter(function(option) { return option.key === select.value; })[0];
    return match ? match.label : '';
}

function _netdlgSelectSet(id, value) {
    _netdlgSelect(id).value = value;
    var el = document.getElementById(id);
    var shown = el && el.querySelector('.v');
    if (shown) shown.textContent = _netdlgSelectLabel(id);
}

function _netdlgSelectOptions(id, options) {
    _netdlgSelect(id).options = options;
    _netdlgSelectSet(id, _netdlgSelectGet(id));
}

function _netdlgSelectField(id, label, help, attrs) {
    return '<div class="ui-field"' + (attrs || '') + '><span class="ui-label" id="' + id + '-label">' + label + '</span>' +
        '<button type="button" class="ui-select ui-select--field" id="' + id + '" data-netdlg="select" aria-haspopup="menu" aria-expanded="false" aria-labelledby="' + id + '-label ' + id + '"' +
            (help ? ' aria-describedby="' + id + '-help"' : '') + '><span class="v">' + _netdlgEsc(_netdlgSelectLabel(id)) + '</span>' + _netdlgIcon('updown') + '</button>' +
        (help ? '<p class="ui-help" id="' + id + '-help">' + help + '</p>' : '') + '</div>';
}

function _netdlgSelectOpen(el) {
    var id = el.id;
    var select = _netdlgSelect(id);
    var label = document.getElementById(id + '-label');
    RS.kit.menu(el, {
        title: label ? label.textContent : '',
        hints: true,
        items: select.options.map(function(option) {
            return {
                label: option.label,
                hint: option.hint,
                checked: option.key === select.value,
                onSelect: function() {
                    _netdlgSelectSet(id, option.key);
                    if (typeof select.onPick === 'function') select.onPick(option.key);
                }
            };
        })
    });
}

function _netdlgFoot(primary, o) {
    o = o || {};
    return (o.back
        ? '<button type="button" class="ui-btn ui-btn--quiet" data-netdlg="back">Back</button>'
        : '<button type="button" class="ui-btn ui-btn--quiet ui-desk-only" data-ui-close>Cancel</button>') +
        '<button type="button" class="ui-btn ui-btn--primary" data-netdlg="submit"' + (o.disabled ? ' disabled' : '') + '>' + _netdlgEsc(primary) + '</button>';
}

function _netdlgPrimary(dlg) {
    return dlg && dlg.foot ? dlg.foot.querySelector('[data-netdlg="submit"]') : null;
}

// A spinner and a short label while the Primary waits; null puts it back.
function _netdlgBusy(btn, label) {
    if (!btn) return;
    if (label) {
        if (!btn.hasAttribute('data-rest')) btn.setAttribute('data-rest', btn.textContent);
        btn.disabled = true;
        btn.innerHTML = '<span class="ui-spin" aria-hidden="true"></span>' + _netdlgEsc(label);
    } else if (btn.hasAttribute('data-rest')) {
        btn.textContent = btn.getAttribute('data-rest');
        btn.removeAttribute('data-rest');
        btn.disabled = false;
    }
}

function _netdlgNoticeSlot() {
    return '<div class="ui-notice ui-notice--error" data-netdlg-notice role="alert" hidden>' + _netdlgIcon('alert') + '<div class="ui-notice-body"></div></div>';
}

// A refusal that belongs to no one field: the notice at the end of the dialog, in view.
function _netdlgNotice(dlg, message) {
    var notice = dlg && dlg.body && dlg.isOpen() ? dlg.body.querySelector('[data-netdlg-notice]') : null;
    if (!notice) {
        if (message) showToast(message, 'toast-error', 8000);
        return;
    }
    notice.hidden = !message;
    notice.querySelector('.ui-notice-body').textContent = message || '';
    if (message && typeof notice.scrollIntoView === 'function') notice.scrollIntoView({ block: 'nearest' });
}

function _netdlgSetError(id, message) {
    var input = document.getElementById(id);
    var slot = document.getElementById(id + '-error');
    // A field another module renders gets its slot on first use.
    if (!slot && message && input) {
        slot = document.createElement('p');
        slot.className = 'ui-error';
        slot.id = id + '-error';
        slot.setAttribute('role', 'alert');
        var after = input.closest('.ui-input-wrap') || input;
        after.parentNode.insertBefore(slot, after.nextSibling);
    }
    if (slot) {
        slot.hidden = !message;
        slot.innerHTML = message ? _netdlgIcon('alert') + '<span>' + _netdlgEsc(message) + '</span>' : '';
    }
    if (input) {
        var help = document.getElementById(id + '-help');
        var described = [help ? help.id : '', message && slot ? slot.id : ''].filter(Boolean).join(' ');
        input.classList.toggle('is-invalid', !!message);
        if (message) input.setAttribute('aria-invalid', 'true');
        else input.removeAttribute('aria-invalid');
        if (described) input.setAttribute('aria-describedby', described);
        else input.removeAttribute('aria-describedby');
    }
    return !message;
}

// Shows a field's error and takes the user to it.
function _netdlgFieldError(id, message) {
    _netdlgSetError(id, message);
    var input = document.getElementById(id);
    if (input) {
        try { input.focus({ preventScroll: true }); } catch (_) {}
        if (typeof input.scrollIntoView === 'function') input.scrollIntoView({ block: 'nearest' });
    }
    return false;
}

function _netdlgClearErrors(dlg) {
    if (!dlg || !dlg.dialog) return;
    dlg.dialog.querySelectorAll('.ui-input.is-invalid').forEach(function(input) { _netdlgSetError(input.id, ''); });
    _netdlgNotice(dlg, '');
}

// A blank port is the default; anything else must be a whole number from 1 to 65535.
function _netdlgPort(id, fallback) {
    var el = document.getElementById(id);
    var raw = el ? String(el.value).trim() : '';
    if (!raw) return fallback;
    if (!/^\d+$/.test(raw)) return null;
    var port = parseInt(raw, 10);
    return port >= 1 && port <= 65535 ? port : null;
}

function _netdlgValue(id) {
    var el = document.getElementById(id);
    return el ? String(el.value).trim() : '';
}

// Backbone clients and servers are desktop only.
function _netdlgBackboneAvailable() {
    return !!window.__RATSPEAK_DESKTOP__;
}

// A re-render keeps keyboard focus on the control that held it.
function _netdlgRerender(container, html) {
    if (!container) return;
    var active = document.activeElement;
    var key = null;
    if (active && container.contains(active)) {
        key = active.id ? '#' + active.id
            : active.getAttribute('data-netdlg') ? '[data-netdlg="' + active.getAttribute('data-netdlg') + '"]' + (active.hasAttribute('data-i') ? '[data-i="' + active.getAttribute('data-i') + '"]' : '')
            : active.getAttribute('data-v') ? '[data-v="' + active.getAttribute('data-v') + '"]'
            : active.getAttribute('data-hd') ? '[data-hd="' + active.getAttribute('data-hd') + '"]' : null;
    }
    container.innerHTML = html;
    var again = key ? container.querySelector(key) : null;
    if (again && !again.disabled) {
        try { again.focus({ preventScroll: true }); } catch (_) {}
    }
}

function _netdlgOpError(data) {
    var detail = data && typeof data.error === 'string' ? data.error : '';
    if (!detail) return (data && data.step) || 'That didn’t work. Try again.';
    return data.step && data.step !== detail ? data.step + ': ' + detail : detail;
}

// Developer-only fields follow the setting while a dialog is open.
window.addEventListener('ratspeak-developer-mode-changed', function() {
    var dev = _developerModeEnabled();
    document.querySelectorAll('.netdlg [data-netdlg-dev]').forEach(function(el) { el.hidden = !dev; });
});

// Network identity changes redraw Host setup's discovery switches (the inline offer leaves).
var _netdlgIdentityBound = false;
function _netdlgBindIdentity() {
    if (_netdlgIdentityBound || !RS.networkIdentity || typeof RS.networkIdentity.onChange !== 'function') return;
    _netdlgIdentityBound = true;
    RS.networkIdentity.onChange(function() { _hostRenderDiscovery(); });
}

var _rnodeConnectionType = 'serial';
var RNODE_TCP_DEFAULT_PORT = 7633;
var _bleSelectedDevice = null;
var _androidUsbSelectedDevice = null;
var _selectedSerialPort = null;
var _rnodeEditContext = null;
var _RNODE_CUSTOM_REGION_KEY = 'custom';
var _RNODE_CUSTOM_PRESET_KEY = 'custom';

function _ifaceString(iface, key, fallback) {
    var v = iface && iface[key];
    if (v === undefined || v === null || v === '') return fallback || '';
    return String(v);
}

function _ifaceInt(iface, key, fallback) {
    var v = parseInt(_ifaceString(iface, key, ''), 10);
    return isNaN(v) ? fallback : v;
}

function _ifaceFloat(iface, key, fallback) {
    var v = parseFloat(_ifaceString(iface, key, ''));
    return isNaN(v) ? fallback : v;
}

function _ifaceBool(iface, key, fallback) {
    var v = _ifaceString(iface, key, '').trim().toLowerCase();
    if (!v) return !!fallback;
    return v === 'true' || v === 'yes' || v === '1' || v === 'on';
}

function _rnodeFormatScaledValue(value, divisor, maxDecimals, minDecimals) {
    var scaled = (parseInt(value, 10) || 0) / divisor;
    var parts = scaled.toFixed(maxDecimals).split('.');
    var frac = (parts[1] || '').replace(/0+$/, '');
    while (frac.length < minDecimals) frac += '0';
    return frac ? parts[0] + '.' + frac : parts[0];
}

function _rnodeFormatMhz(freq) {
    return _rnodeFormatScaledValue(freq, 1000000, 6, 3);
}

function _rnodeFormatKhz(bw) {
    return _rnodeFormatScaledValue(bw, 1000, 3, 0);
}

function _rnodeParseScaledInput(raw, decimalUnit, integerThreshold, multiplier) {
    var text = String(raw || '').trim().toLowerCase().replace(/[, _]/g, '');
    if (!text) return null;
    var explicitDecimalUnit = text.endsWith(decimalUnit);
    var explicitHz = text.endsWith('hz') && !explicitDecimalUnit;
    if (explicitDecimalUnit) text = text.slice(0, -decimalUnit.length);
    else if (explicitHz) text = text.slice(0, -2);
    var value = Number(text);
    if (!isFinite(value) || value <= 0) return null;
    if (explicitDecimalUnit || (!explicitHz && value < integerThreshold)) {
        return Math.round(value * multiplier);
    }
    return Math.round(value);
}

function _rnodeParseFrequencyHz(raw) {
    return _rnodeParseScaledInput(raw, 'mhz', 10000, 1000000);
}

function _rnodeParseBandwidthHz(raw) {
    return _rnodeParseScaledInput(raw, 'khz', 10000, 1000);
}

function _rnodeRegionContainsFrequency(regionKey, freq) {
    var region = _rnodeRegions && _rnodeRegions[regionKey];
    return !!region && region.min <= freq && freq <= region.max;
}

function _rnodeRegionForFrequency(freq) {
    var keys = Object.keys(_rnodeRegions || {});
    for (var i = 0; i < keys.length; i++) {
        if (_rnodeRegions[keys[i]].freq === freq) return keys[i];
    }
    for (var j = 0; j < keys.length; j++) {
        if (_rnodeRegionContainsFrequency(keys[j], freq)) return keys[j];
    }
    return _RNODE_CUSTOM_REGION_KEY;
}

function _rnodeRegionForInterface(iface, freq) {
    var key = _ifaceString(iface, 'ratspeak_region', '');
    if (key && _rnodeRegionContainsFrequency(key, freq)) return key;
    return _rnodeRegionForFrequency(freq);
}

function _rnodePresetMatchesParams(presetKey, bw, sf, cr, tx) {
    var p = _rnodePresets && _rnodePresets[presetKey];
    return !!p && p.bw === bw && p.sf === sf && p.cr === cr && p.tx === tx;
}

function _rnodePresetForParams(bw, sf, cr, tx) {
    var keys = Object.keys(_rnodePresets || {});
    for (var i = 0; i < keys.length; i++) {
        if (_rnodePresetMatchesParams(keys[i], bw, sf, cr, tx)) return keys[i];
    }
    return _RNODE_CUSTOM_PRESET_KEY;
}

function _rnodePresetForInterface(iface, bw, sf, cr, tx) {
    var key = _ifaceString(iface, 'ratspeak_preset', '');
    if (key && _rnodePresetMatchesParams(key, bw, sf, cr, tx)) return key;
    return _rnodePresetForParams(bw, sf, cr, tx);
}

function _rnodeSetFrequency(freq) {
    var input = document.getElementById('rnode-frequency');
    if (input) input.value = _rnodeFormatMhz(freq);
}

function _rnodeSetAdvancedParams(bw, sf, cr, tx) {
    var bwInput = document.getElementById('rnode-bandwidth');
    var sfInput = document.getElementById('rnode-spreading-factor');
    var crInput = document.getElementById('rnode-coding-rate');
    var txInput = document.getElementById('rnode-tx-power');
    if (bwInput) bwInput.value = _rnodeFormatKhz(bw);
    if (sfInput) sfInput.value = String(sf);
    if (crInput) crInput.value = String(cr);
    if (txInput) txInput.value = String(tx);
}

function _rnodeApplyPresetToAdvanced(presetKey) {
    var preset = _rnodePresets && _rnodePresets[presetKey];
    if (!preset) return;
    _rnodeSetAdvancedParams(preset.bw, preset.sf, preset.cr, preset.tx);
    _rnodeUpdateRadioHints();
}

function _rnodeApplyRegionToFrequency(regionKey) {
    var region = _rnodeRegions && _rnodeRegions[regionKey];
    if (!region) return;
    _rnodeSetFrequency(region.freq);
    _rnodeUpdateRadioHints();
}

function _rnodeSelectedRegionKey() {
    return _netdlgSelectGet('rnode-region') || _rnodeCatalogDefaults.region || 'americas';
}

function _rnodeSelectedPresetKey() {
    return _netdlgSelectGet('rnode-preset') || _rnodeCatalogDefaults.preset || 'medium_fast';
}

// Empty input = no duty-cycle limit; returns null for empty, NaN for garbage.
function _rnodeParseAirtimePercent(id) {
    var el = document.getElementById(id);
    var raw = el ? String(el.value).trim() : '';
    if (!raw) return null;
    return parseFloat(raw);
}

function _rnodeSetAirtimeLimits(shortVal, longVal) {
    var shortInput = document.getElementById('rnode-airtime-short');
    var longInput = document.getElementById('rnode-airtime-long');
    if (shortInput) shortInput.value = shortVal === null || shortVal === undefined ? '' : String(shortVal);
    if (longInput) longInput.value = longVal === null || longVal === undefined ? '' : String(longVal);
}

function _rnodeReadRadioSettings() {
    var freq = _rnodeParseFrequencyHz(document.getElementById('rnode-frequency').value);
    var bw = _rnodeParseBandwidthHz(document.getElementById('rnode-bandwidth').value);
    var sf = parseInt(document.getElementById('rnode-spreading-factor').value, 10);
    var cr = parseInt(document.getElementById('rnode-coding-rate').value, 10);
    var tx = parseInt(document.getElementById('rnode-tx-power').value, 10);
    var limits = _rnodeRadioLimits || {};
    if (!freq || freq < limits.frequencyMin || freq > limits.frequencyMax) {
        return { field: 'rnode-frequency', error: 'Frequency must be between ' + _rnodeFormatMhz(limits.frequencyMin) + ' and ' + _rnodeFormatMhz(limits.frequencyMax) + ' MHz' };
    }
    if (!bw || bw < limits.bandwidthMin || bw > limits.bandwidthMax) {
        return { field: 'rnode-bandwidth', error: 'Bandwidth must be between ' + _rnodeFormatKhz(limits.bandwidthMin) + ' and ' + _rnodeFormatKhz(limits.bandwidthMax) + ' kHz' };
    }
    if (isNaN(sf) || sf < limits.sfMin || sf > limits.sfMax) {
        return { field: 'rnode-spreading-factor', error: 'Spreading factor must be between ' + limits.sfMin + ' and ' + limits.sfMax };
    }
    if (isNaN(cr) || cr < limits.crMin || cr > limits.crMax) {
        return { field: 'rnode-coding-rate', error: 'Coding rate must be between ' + limits.crMin + ' and ' + limits.crMax };
    }
    if (isNaN(tx) || tx < limits.txMin || tx > limits.txMax) {
        return { field: 'rnode-tx-power', error: 'TX power must be between ' + limits.txMin + ' and ' + limits.txMax + ' dBm' };
    }
    var airtimeShort = _rnodeParseAirtimePercent('rnode-airtime-short');
    var airtimeLong = _rnodeParseAirtimePercent('rnode-airtime-long');
    if (airtimeShort !== null && (isNaN(airtimeShort) || airtimeShort < 0 || airtimeShort > 100)) {
        return { field: 'rnode-airtime-short', error: 'Short-term airtime limit must be between 0 and 100 %' };
    }
    if (airtimeLong !== null && (isNaN(airtimeLong) || airtimeLong < 0 || airtimeLong > 100)) {
        return { field: 'rnode-airtime-long', error: 'Long-term airtime limit must be between 0 and 100 %' };
    }

    var regionKey = _rnodeSelectedRegionKey();
    var presetKey = _rnodeSelectedPresetKey();
    var region = _rnodeRegions[regionKey];
    var preset = _rnodePresets[presetKey];
    var customParams = !region || !preset || freq !== region.freq ||
        !_rnodePresetMatchesParams(presetKey, bw, sf, cr, tx);

    return {
        regionKey: regionKey,
        presetKey: presetKey,
        frequency: freq,
        bandwidth: bw,
        spreadingFactor: sf,
        codingRate: cr,
        txPower: tx,
        customParams: customParams,
        airtimeShort: airtimeShort,
        airtimeLong: airtimeLong,
    };
}

function _rnodeApproxCoordinate(value) {
    var rounded = Math.round(value * 1000) / 1000;
    return Object.is(rounded, -0) ? 0 : rounded;
}

function _rnodePublicMapElements() {
    return {
        section: document.getElementById('rnode-public-map-section'),
        checkbox: document.getElementById('rnode-public-map-enabled'),
        controls: document.getElementById('rnode-public-map-controls'),
        status: document.getElementById('rnode-public-map-status'),
        latitude: document.getElementById('rnode-public-map-latitude'),
        longitude: document.getElementById('rnode-public-map-longitude'),
        error: document.getElementById('rnode-public-map-error'),
    };
}

function _rnodeSetPublicMapError(message) {
    var error = document.getElementById('rnode-public-map-error');
    if (!error) return;
    error.hidden = !message;
    error.innerHTML = message ? RS.kit.icon('alert') + '<span>' + escapeHtml(message) + '</span>' : '';
}

function _rnodeSetPublicMapStatus() {
    var els = _rnodePublicMapElements();
    if (!els.status) return;
    var latRaw = els.latitude ? String(els.latitude.value).trim() : '';
    var lonRaw = els.longitude ? String(els.longitude.value).trim() : '';
    var lat = latRaw ? Number(latRaw) : NaN;
    var lon = lonRaw ? Number(lonRaw) : NaN;
    if (isNaN(lat) || isNaN(lon)) {
        els.status.textContent = 'Enter latitude and longitude manually.';
        return;
    }
    var latText = String(_rnodeApproxCoordinate(lat));
    var lonText = String(_rnodeApproxCoordinate(lon));
    els.status.innerHTML = 'Approx. <code>' + escapeHtml(latText + ', ' + lonText) + '</code>';
}

function _rnodeSetPublicMapEnabled(enabled) {
    var els = _rnodePublicMapElements();
    if (els.checkbox) els.checkbox.checked = !!enabled;
    if (els.controls) els.controls.hidden = !enabled;
    if (!enabled) {
        _rnodeSetPublicMapError('');
    } else {
        _rnodeSetPublicMapStatus();
    }
}

function _rnodeResetPublicMap() {
    var els = _rnodePublicMapElements();
    if (els.latitude) els.latitude.value = '';
    if (els.longitude) els.longitude.value = '';
    _rnodeSetPublicMapError('');
    _rnodeSetPublicMapEnabled(false);
}

// The public map is offered only when editing a radio, so its section exists only then.
function _rnodeLoadPublicMap(editIface) {
    var els = _rnodePublicMapElements();
    if (!els.section) return;
    if (!editIface) {
        _rnodeSetPublicMapEnabled(false);
        return;
    }
    var discoverable = _ifaceBool(editIface, 'discoverable', false);
    var lat = _ifaceFloat(editIface, 'latitude', null);
    var lon = _ifaceFloat(editIface, 'longitude', null);
    if (els.latitude) els.latitude.value = lat === null ? '' : String(_rnodeApproxCoordinate(lat));
    if (els.longitude) els.longitude.value = lon === null ? '' : String(_rnodeApproxCoordinate(lon));
    _rnodeSetPublicMapEnabled(discoverable);
}

function _rnodeParsePublicMapLocation() {
    var els = _rnodePublicMapElements();
    var latRaw = els.latitude ? String(els.latitude.value).trim() : '';
    var lonRaw = els.longitude ? String(els.longitude.value).trim() : '';
    if (!latRaw || !lonRaw) return { error: 'Add a location before enabling public map.' };
    var lat = Number(latRaw);
    var lon = Number(lonRaw);
    if (!isFinite(lat)) return { error: 'Latitude must be between -90 and 90.' };
    if (!isFinite(lon)) return { error: 'Longitude must be between -180 and 180.' };
    if (lat < -90 || lat > 90) return { error: 'Latitude must be between -90 and 90.' };
    if (lon < -180 || lon > 180) return { error: 'Longitude must be between -180 and 180.' };
    lat = _rnodeApproxCoordinate(lat);
    lon = _rnodeApproxCoordinate(lon);
    if (els.latitude) els.latitude.value = String(lat);
    if (els.longitude) els.longitude.value = String(lon);
    return { latitude: lat, longitude: lon };
}

function _rnodeActiveIdentityDisplayName() {
    if (typeof activeIdentity === 'function') {
        var active = activeIdentity();
        if (active && active.display_name) return String(active.display_name).trim();
    }
    try {
        return String(localStorage.getItem('ratspeak_identity_name') || '').trim();
    } catch (e) {
        return '';
    }
}

function _rnodeReadPublicMapSettings() {
    var els = _rnodePublicMapElements();
    if (!els.section || !els.checkbox || !els.checkbox.checked) {
        return { enabled: false };
    }
    if (!_rnodeActiveIdentityDisplayName()) {
        return { error: 'Set an identity display name before enabling public map.' };
    }
    var location = _rnodeParsePublicMapLocation();
    if (location.error) return location;
    return {
        enabled: true,
        latitude: location.latitude,
        longitude: location.longitude,
    };
}

function _rnodeRequestPublicMapLocation() {
    var els = _rnodePublicMapElements();
    _rnodeSetPublicMapError('');
    if (els.status) els.status.textContent = 'Requesting current approximate location...';
    if (!navigator.geolocation) {
        _rnodeSetPublicMapError('Location unavailable. Enter latitude and longitude manually.');
        _rnodeSetPublicMapStatus();
        return;
    }
    navigator.geolocation.getCurrentPosition(function(pos) {
        var coords = pos && pos.coords ? pos.coords : {};
        var lat = typeof coords.latitude === 'number' ? coords.latitude : NaN;
        var lon = typeof coords.longitude === 'number' ? coords.longitude : NaN;
        if (!isFinite(lat) || !isFinite(lon)) {
            _rnodeSetPublicMapError('Location unavailable. Enter latitude and longitude manually.');
            _rnodeSetPublicMapStatus();
            return;
        }
        if (els.latitude) els.latitude.value = String(_rnodeApproxCoordinate(lat));
        if (els.longitude) els.longitude.value = String(_rnodeApproxCoordinate(lon));
        _rnodeSetPublicMapError('');
        _rnodeSetPublicMapStatus();
    }, function(err) {
        var denied = err && err.code === 1;
        _rnodeSetPublicMapError(denied
            ? 'Location permission was denied. Enter latitude and longitude manually.'
            : 'Location unavailable. Enter latitude and longitude manually.');
        _rnodeSetPublicMapStatus();
    }, {
        enableHighAccuracy: false,
        timeout: 12000,
        maximumAge: 600000,
    });
}

function _rnodeEnablePublicMapWithWarning() {
    var warning = "This node's approximate location data will be broadcast publicly. The location will be your current approximate location, and only change again if you update it. Location is never live tracked.";
    if (typeof rsConfirm !== 'function') {
        _rnodeSetPublicMapEnabled(false);
        return;
    }
    rsConfirm({
        title: 'Display on public map?',
        message: warning,
        confirmText: 'Enable',
        cancelText: 'Cancel',
    }).then(function(ok) {
        if (!ok) {
            _rnodeSetPublicMapEnabled(false);
            return;
        }
        _rnodeSetPublicMapEnabled(true);
        _rnodeRequestPublicMapLocation();
    });
}

// The design carries no frequency or preset hint lines; the out-of-band warning stays.
function _rnodeUpdateRadioHints() {
    var input = document.getElementById('rnode-frequency');
    var warn = document.getElementById('rnode-frequency-warning');
    if (!input || !warn) return;
    var freq = _rnodeParseFrequencyHz(input.value);
    var regionKey = _rnodeSelectedRegionKey();
    var region = _rnodeRegions && _rnodeRegions[regionKey];
    var outside = !!(freq && region && !_rnodeRegionContainsFrequency(regionKey, freq));
    warn.hidden = !outside;
    warn.querySelector('.ui-notice-body').textContent = outside
        ? 'This frequency is outside the selected band range of ' + _rnodeFormatMhz(region.min) + '-' + _rnodeFormatMhz(region.max) +
            ' MHz. Check hardware support and local regulations before transmitting.'
        : '';
}

function _rnodeRefreshRegionFromFrequency() {
    var freq = _rnodeParseFrequencyHz(document.getElementById('rnode-frequency').value);
    var current = _netdlgSelectGet('rnode-region');
    if (freq && !(current !== _RNODE_CUSTOM_REGION_KEY && _rnodeRegionContainsFrequency(current, freq))) {
        _netdlgSelectSet('rnode-region', _rnodeRegionForFrequency(freq));
    }
    _rnodeUpdateRadioHints();
}

function _rnodeRefreshPresetFromAdvanced() {
    var bw = _rnodeParseBandwidthHz(document.getElementById('rnode-bandwidth').value);
    var sf = parseInt(document.getElementById('rnode-spreading-factor').value, 10);
    var cr = parseInt(document.getElementById('rnode-coding-rate').value, 10);
    var tx = parseInt(document.getElementById('rnode-tx-power').value, 10);
    if (bw && !isNaN(sf) && !isNaN(cr) && !isNaN(tx)) _netdlgSelectSet('rnode-preset', _rnodePresetForParams(bw, sf, cr, tx));
    _rnodeUpdateRadioHints();
}

function _rnodeApplyRadioControls(regionKey, presetKey, freq, bw, sf, cr, tx, openAdvanced) {
    _netdlgSelectSet('rnode-region', regionKey || _rnodeCatalogDefaults.region || 'americas');
    _netdlgSelectSet('rnode-preset', presetKey || _rnodeCatalogDefaults.preset || 'medium_fast');
    _rnodeSetFrequency(freq);
    _rnodeSetAdvancedParams(bw, sf, cr, tx);
    _netdlgSetDisclosure('rnode-advanced', !!openAdvanced);
    _rnodeUpdateRadioHints();
}

function _rnodeApplyDefaultRadioControls() {
    var regionKey = _rnodeCatalogDefaults.region || 'americas';
    var presetKey = _rnodeCatalogDefaults.preset || 'medium_fast';
    var region = _rnodeRegions && _rnodeRegions[regionKey];
    var preset = _rnodePresets && _rnodePresets[presetKey];
    _rnodeApplyRadioControls(
        regionKey,
        presetKey,
        region ? region.freq : 915000000,
        preset ? preset.bw : 250000,
        preset ? preset.sf : 9,
        preset ? preset.cr : 5,
        preset ? preset.tx : 17,
        false
    );
}

function _rnodeIsTcpPort(port) {
    return ((port || '').slice(0, 6).toLowerCase() === 'tcp://');
}

function _rnodeValidateTcpHost(host) {
    return !!host && !/[\s\/\?#\[\]\x00-\x1f\x7f]/.test(host);
}

function _rnodeParseTcpPort(portText) {
    if (!portText) return { error: 'Missing TCP port.' };
    if (!/^\d+$/.test(portText)) return { error: 'Invalid TCP port.' };
    var port = parseInt(portText, 10);
    if (port < 0 || port > 65535) return { error: 'TCP port must be 0-65535.' };
    return { port: String(port) };
}

function _normaliseRnodeTcpEndpoint(raw) {
    var endpoint = (raw || '').trim();
    if (_rnodeIsTcpPort(endpoint)) endpoint = endpoint.slice(6).trim();
    if (!endpoint) return { error: 'Enter a TCP endpoint.' };

    if (endpoint.charAt(0) === '[') {
        var close = endpoint.indexOf(']');
        if (close < 0) return { error: 'Missing closing ] in IPv6 address.' };
        var bracketHost = endpoint.slice(1, close);
        if (!_rnodeValidateTcpHost(bracketHost)) return { error: 'Invalid TCP host.' };
        var tail = endpoint.slice(close + 1);
        var bracketPort = String(RNODE_TCP_DEFAULT_PORT);
        if (tail) {
            if (tail.charAt(0) !== ':') return { error: 'Unexpected text after IPv6 address.' };
            var parsedBracketPort = _rnodeParseTcpPort(tail.slice(1));
            if (parsedBracketPort.error) return parsedBracketPort;
            bracketPort = parsedBracketPort.port;
        }
        var bracketLabel = '[' + bracketHost + ']:' + bracketPort;
        return { port: 'tcp://' + bracketLabel, label: bracketLabel };
    }

    if (!_rnodeValidateTcpHost(endpoint)) return { error: 'Invalid TCP host.' };
    var colonMatches = endpoint.match(/:/g);
    var colonCount = colonMatches ? colonMatches.length : 0;
    if (colonCount === 0) {
        var defaultLabel = endpoint + ':' + RNODE_TCP_DEFAULT_PORT;
        return { port: 'tcp://' + defaultLabel, label: defaultLabel };
    }
    if (colonCount === 1) {
        var parts = endpoint.split(':');
        if (!_rnodeValidateTcpHost(parts[0])) return { error: 'Invalid TCP host.' };
        var parsedPort = _rnodeParseTcpPort(parts[1]);
        if (parsedPort.error) return parsedPort;
        var label = parts[0] + ':' + parsedPort.port;
        return { port: 'tcp://' + label, label: label };
    }

    var ipv6Label = '[' + endpoint + ']:' + RNODE_TCP_DEFAULT_PORT;
    return { port: 'tcp://' + ipv6Label, label: ipv6Label };
}

function _rnodeTcpInputValue(port) {
    if (_rnodeIsTcpPort(port)) return port.slice(6);
    return port || '';
}

function _rnodeModeForPort(port) {
    if (_rnodeIsTcpPort(port)) return 'tcp';
    if ((port || '').indexOf('ble://') === 0) return 'ble';
    if ((port || '').indexOf('androidusb://') === 0) return 'android-usb';
    return 'serial';
}

var _RNODE_INTERFACE_MODE_VALUES = {
    full: true,
    gateway: true,
    access_point: true,
    boundary: true,
    roaming: true,
};
var _RNODE_NEW_INTERFACE_MODE = 'roaming';
var _RNODE_LEGACY_INTERFACE_MODE = 'full';

function _rnodeNormaliseInterfaceMode(mode) {
    mode = String(mode || 'full').trim().toLowerCase();
    if (mode === 'gw') mode = 'gateway';
    if (mode === 'ap' || mode === 'accesspoint' || mode === 'access point') mode = 'access_point';
    return _RNODE_INTERFACE_MODE_VALUES[mode] ? mode : 'full';
}

function _rnodeSetInterfaceMode(mode) {
    _netdlgSelectSet('rnode-interface-mode', _rnodeNormaliseInterfaceMode(mode));
}

function _rnodeReadInterfaceMode() {
    return _rnodeNormaliseInterfaceMode(_netdlgSelectGet('rnode-interface-mode') || 'full');
}

function _rnodeInitialInterfaceMode(editIface) {
    if (!editIface) return _RNODE_NEW_INTERFACE_MODE;
    return editIface.mode || editIface.interface_mode || _RNODE_LEGACY_INTERFACE_MODE;
}

function _rnodeDeveloperModeEnabled() {
    return typeof window.ratspeakDeveloperModeEnabled === 'function' &&
        window.ratspeakDeveloperModeEnabled();
}

function _developerModeEnabled() {
    return _rnodeDeveloperModeEnabled();
}

// ------------------------------------------------------------------ Set up a radio
// Two steps: how the radio is connected (Bluetooth, USB, or Wi-Fi), then its radio settings.
var _rnodeDialog = null;
var _rnodeStep = 1;
var _rnodePrefill = null;
var _rnodePrefillName = '';
var _rnodeTcpDraft = '';
// Radios found per connection: { state: 'idle' | 'scanning' | 'done' | 'error', devices, message }.
var _rnodeFound = {};

_netdlgSelectOptions('rnode-interface-mode', [
    { key: 'full', label: 'Full' },
    { key: 'gateway', label: 'Gateway' },
    { key: 'access_point', label: 'Access point (AP)' },
    { key: 'boundary', label: 'Boundary' },
    { key: 'roaming', label: 'Roaming (recommended)' }
]);
_netdlgSelect('rnode-region').onPick = function(key) {
    if (key !== _RNODE_CUSTOM_REGION_KEY) _rnodeApplyRegionToFrequency(key);
    else _rnodeUpdateRadioHints();
};
_netdlgSelect('rnode-preset').onPick = function(key) {
    if (key !== _RNODE_CUSTOM_PRESET_KEY) {
        _rnodeApplyPresetToAdvanced(key);
    } else {
        _netdlgSetDisclosure('rnode-advanced', true);
        _rnodeUpdateRadioHints();
    }
};

// iOS MFi blocks USB serial; Android uses USB-OTG JNI; desktop uses serialport.
function _rnodeConnectionTypes() {
    var types = [['ble', 'Bluetooth']];
    if (isAndroid() && hasAndroidBridge()) types.push(['android-usb', 'USB']);
    else if (!isIOS()) types.push(['serial', 'USB']);
    types.push(['tcp', 'Wi-Fi']);
    return types;
}

function _rnodeCanScanBle() {
    return !!(window._bleAvailable || window._blePromptPending || hasAndroidBridge());
}

function _rnodeBleNoticeHtml() {
    if (_rnodeConnectionType !== 'ble' || _rnodeCanScanBle()) return '';
    var denied = window._bleAuthState === 'denied' || window._bleAuthState === 'restricted';
    var reason = denied ? 'Ratspeak isn’t allowed to use Bluetooth.'
        : ((window._bleMissing || [])[0] || 'Bluetooth isn’t available on this device.');
    return '<div class="ui-notice ui-notice--warning">' + _netdlgIcon('alert') + '<div class="ui-notice-body">' + _netdlgEsc(reason) + '</div>' +
        (denied ? '<button type="button" class="ui-btn ui-btn--sm" data-netdlg="ble-settings">Open Settings</button>' : '') + '</div>';
}

function _rnodeSelected(type) {
    if (type === 'ble') return _bleSelectedDevice;
    if (type === 'android-usb') return _androidUsbSelectedDevice;
    return _selectedSerialPort;
}

function _rnodeDeviceKey(type, dev) {
    if (!dev) return '';
    if (type === 'ble') return dev.address || '';
    if (type === 'android-usb') return dev.device_name || '';
    return dev.device || '';
}

// A scan keeps the radio already chosen, even when it is out of range this time.
function _rnodeKeepSelected(type, devices) {
    var selected = _rnodeSelected(type);
    var key = _rnodeDeviceKey(type, selected);
    if (!key || devices.some(function(dev) { return _rnodeDeviceKey(type, dev) === key; })) return devices;
    return [selected].concat(devices);
}

function _rnodeSignal(rssi) {
    if (rssi === null || rssi === undefined) return '';
    return rssi > -64 ? 'Strong signal' : (rssi > -82 ? 'Good signal' : 'Weak signal');
}

function _rnodeDeviceText(type, dev, devices) {
    if (type === 'ble') {
        var sub = [_rnodeSignal(dev.rssi)];
        if (_bondBadgeReliable() && typeof dev.bonded === 'boolean') sub.push(dev.bonded ? 'Paired' : 'Not paired');
        var twin = devices.some(function(other) { return other !== dev && other.name === dev.name; });
        if (twin || !sub.filter(Boolean).length) sub.push(dev.address);
        return { title: dev.name || 'RNode', sub: sub.filter(Boolean).join(' · ') };
    }
    if (type === 'android-usb') {
        var hex = function(n) { return Number(n).toString(16).toUpperCase().padStart(4, '0'); };
        return {
            title: dev.product || dev.manufacturer || dev.device_name,
            sub: typeof dev.vid === 'number' ? 'VID ' + hex(dev.vid) + ' · PID ' + hex(dev.pid) : ''
        };
    }
    var described = !!dev.description && dev.description !== dev.device;
    return {
        title: described ? dev.description : dev.device,
        sub: [described ? dev.device : '', dev.permission_denied ? 'Permission denied' : ''].filter(Boolean).join(' · ')
    };
}

function _rnodeFoundHtml(type) {
    var found = _rnodeFound[type] || { state: 'idle', devices: [] };
    if (found.state === 'scanning') {
        return '<div class="ui-empty ui-empty--inline" role="status"><span class="ui-spin ui-spin--ring" aria-hidden="true"></span><span>Looking for radios…</span></div>';
    }
    if (found.state === 'error') {
        return '<div class="ui-empty ui-empty--inline" role="status">' + _netdlgIcon('alert') + '<span>' + _netdlgEsc(found.message) + '</span></div>';
    }
    if (!found.devices.length) {
        var empty = found.state !== 'done' ? 'Scan to find radios nearby.'
            : type === 'ble' ? 'No radios found. Turn on pairing mode, then scan again.' : 'No radios found. Check the cable, then scan again.';
        return '<div class="ui-empty ui-empty--inline" role="status">' + _netdlgIcon('tower') + '<span>' + empty + '</span></div>';
    }
    var selectedKey = _rnodeDeviceKey(type, _rnodeSelected(type));
    return '<div class="netdlg-radios" role="radiogroup" aria-label="Radios found"><ul class="ui-list">' + found.devices.map(function(dev, i) {
        var on = !!selectedKey && _rnodeDeviceKey(type, dev) === selectedKey;
        var text = _rnodeDeviceText(type, dev, found.devices);
        return '<li><button type="button" class="ui-row ui-row--one" role="radio" aria-checked="' + on + '"' + (on ? ' aria-current="true"' : '') +
                ' data-netdlg="radio-pick" data-i="' + i + '">' +
            '<span class="netdlg-mark" aria-hidden="true">' + _netdlgIcon(type === 'ble' ? 'bluetooth' : 'usb') + '</span>' +
            '<span class="ui-row-main"><span class="ui-row-top"><span class="ui-row-title">' + _netdlgEsc(text.title) + '</span></span>' +
                (text.sub ? '<span class="ui-row-sub"><span class="t">' + _netdlgEsc(text.sub) + '</span></span>' : '') + '</span>' +
            (on ? _netdlgIcon('check') : '') + '</button></li>';
    }).join('') + '</ul></div>';
}

function _rnodeScanRowHtml(type) {
    var found = _rnodeFound[type] || { state: 'idle' };
    var scanning = found.state === 'scanning';
    var help = type === 'ble' ? 'Not listed? Turn on pairing mode on the radio.'
        : (type === 'android-usb' ? 'Not listed? Check the OTG cable and that the radio is on.' : 'Not listed? Check the cable.');
    return '<div class="netdlg-scanrow"><p class="ui-help">' + help + '</p>' +
        '<button type="button" class="ui-btn ui-btn--sm" data-netdlg="radio-scan"' + (scanning ? ' disabled' : '') + '>' +
            (scanning ? '<span class="ui-spin" aria-hidden="true"></span>Scanning…' : _netdlgIcon('refresh') + (found.state === 'idle' ? 'Scan' : 'Scan again')) +
        '</button></div>';
}

function _rnodeStep1Html() {
    var type = _rnodeConnectionType;
    var body = type === 'tcp'
        ? _netdlgTextField('rnode-tcp-endpoint', 'Radio address', {
            value: _rnodeTcpDraft, placeholder: '192.168.1.50:7633', mode: 'url', mono: true,
            help: 'Host or host:port. Port 7633 if you leave it out.'
        })
        : _rnodeBleNoticeHtml() + _rnodeFoundHtml(type) + _rnodeScanRowHtml(type);
    return '<p>Step 1 of 2 · How is the radio connected?</p>' +
        _netdlgSeg('rnode-type', _rnodeConnectionTypes(), type, 'How the radio is connected') + body;
}

function _rnodeStep2Html(editIface) {
    return '<p id="rnode-step-2-intro">Step 2 of 2 · Radio settings.</p>' +
        _netdlgTextField('rnode-iface-name', 'Name', { placeholder: 'My LoRa Radio', max: 32 }) +
        _netdlgSelectField('rnode-region', 'Region') +
        _netdlgTextField('rnode-frequency', 'Frequency', { placeholder: '915.000', mode: 'decimal', unit: 'MHz' }) +
        '<div class="ui-notice ui-notice--warning" id="rnode-frequency-warning" hidden>' + _netdlgIcon('alert') + '<div class="ui-notice-body"></div></div>' +
        _netdlgSelectField('rnode-preset', 'Preset', 'Every radio you talk to needs the same settings.') +
        _netdlgSelectField('rnode-interface-mode', 'Interface mode',
            'Best for mobile radios. Choose another mode only when this RNode has a fixed routing role.',
            ' id="rnode-mode-field" data-netdlg-dev' + (_developerModeEnabled() ? '' : ' hidden')) +
        '<div class="ui-group">' +
            _netdlgDisclosure('rnode-advanced', 'Advanced radio settings', 'Bandwidth, spreading factor, coding rate, power, and airtime.', false) +
            '<div class="ui-prop settings-prop-stack" id="rnode-advanced-body" hidden><div class="netdlg-two">' +
                _netdlgTextField('rnode-bandwidth', 'Bandwidth', { placeholder: '250', mode: 'decimal', unit: 'kHz' }) +
                _netdlgTextField('rnode-spreading-factor', 'Spreading factor', { placeholder: '9', mode: 'numeric', max: 2 }) +
                _netdlgTextField('rnode-coding-rate', 'Coding rate', { placeholder: '5', mode: 'numeric', max: 1 }) +
                _netdlgTextField('rnode-tx-power', 'Transmit power', { placeholder: '17', mode: 'numeric', max: 2, unit: 'dBm' }) +
                _netdlgTextField('rnode-airtime-short', 'Airtime limit, short term', { placeholder: 'No limit', mode: 'decimal', unit: '%' }) +
                _netdlgTextField('rnode-airtime-long', 'Airtime limit, long term', { placeholder: 'No limit', mode: 'decimal', unit: '%' }) +
            '</div><p class="ui-help">Change these only when every radio on the link uses the same values.</p></div>' +
        '</div>' +
        (editIface ? _rnodePublicMapHtml() : '') +
        _netdlgNoticeSlot();
}

function _rnodePublicMapHtml() {
    return '<div class="ui-group" id="rnode-public-map-section">' +
        _netdlgSwitch('rnode-public-map-enabled', 'Display on public map', 'Broadcasts this radio’s approximate location.', false) +
        '<div class="ui-prop settings-prop-stack" id="rnode-public-map-controls" hidden><div class="netdlg-two">' +
            _netdlgTextField('rnode-public-map-latitude', 'Latitude', { placeholder: '39.7392' }) +
            _netdlgTextField('rnode-public-map-longitude', 'Longitude', { placeholder: '-104.9903' }) +
        '</div><div class="netdlg-scanrow"><p class="ui-help" id="rnode-public-map-status" aria-live="polite"></p>' +
            '<button type="button" class="ui-btn ui-btn--sm" id="rnode-public-map-use-current">Use current location</button></div>' +
        '<p class="ui-error" id="rnode-public-map-error" role="alert" hidden></p></div></div>';
}

function _rnodeRenderStep1() {
    if (!_rnodeDialog || !_rnodeDialog.isOpen()) return;
    _netdlgRerender(document.getElementById('rnode-step-1'), _rnodeStep1Html());
    rnodeUpdateNextBtn();
}

function _rnodeRenderFoot() {
    if (!_rnodeDialog || !_rnodeDialog.foot) return;
    _rnodeDialog.foot.innerHTML = _rnodeStep === 1
        ? _netdlgFoot('Next', { disabled: true })
        : _netdlgFoot(_rnodeEditContext ? 'Save' : 'Connect radio', { back: true });
    rnodeUpdateNextBtn();
}

function _rnodeShowStep(step) {
    _rnodeStep = step;
    document.getElementById('rnode-step-1').hidden = step !== 1;
    document.getElementById('rnode-step-2').hidden = step !== 2;
    if (step === 1) _rnodeRenderStep1();
    _rnodeRenderFoot();
    if (_rnodeDialog.body) _rnodeDialog.body.scrollTop = 0;
}

// The foot is redrawn per step, so focus moves into the step instead of falling to the page.
function _rnodeFocusStep() {
    if (!_rnodeDialog || !_rnodeDialog.isOpen()) return;
    var target = !RS.kit.compact() && document.getElementById(_rnodeStep === 2 ? 'rnode-iface-name' : 'rnode-tcp-endpoint');
    target = target || _rnodeDialog.dialog;
    try { target.focus({ preventScroll: true }); } catch (_) {}
}

// Serial and USB lists refresh each time they show; a Bluetooth scan runs once on its own.
function _rnodeEnsureScan(type) {
    if (type === 'serial') refreshRnodeSerialPorts();
    else if (type === 'android-usb') refreshAndroidUsbDevices();
    else if (type === 'ble' && !_rnodeFound.ble && _rnodeCanScanBle()) scanBleDevices();
}

function _rnodeScan(type) {
    if (type === 'ble') scanBleDevices();
    else if (type === 'serial') refreshRnodeSerialPorts();
    else if (type === 'android-usb') refreshAndroidUsbDevices();
}

function openRnodeModal(mode, editIface, prefill) {
    if (_rnodeDialog && _rnodeDialog.isOpen()) _rnodeDialog.close();
    mode = mode || 'ble';
    if (isIOS() && mode !== 'tcp') mode = 'ble';
    if (isAndroid() && hasAndroidBridge() && mode === 'serial') mode = 'android-usb';
    _rnodeConnectionType = mode;
    _rnodeEditContext = null;
    _bleSelectedDevice = null;
    _androidUsbSelectedDevice = null;
    _selectedSerialPort = null;
    _rnodePrefill = editIface ? null : (prefill || null);
    _rnodePrefillName = '';
    _rnodeTcpDraft = '';
    _rnodeFound = {};
    _rnodeStep = editIface ? 2 : 1;

    if (editIface) {
        var port = _ifaceString(editIface, 'port', '');
        _rnodeEditContext = { oldName: editIface.name || '', port: port };
        if (port.indexOf('ble://') === 0) {
            _bleSelectedDevice = { name: editIface.name || 'LoRa Radio', address: port.substring(6) };
            _rnodeFound.ble = { state: 'done', devices: [_bleSelectedDevice] };
        } else if (port.indexOf('androidusb://') === 0) {
            _androidUsbSelectedDevice = { device_name: port.substring('androidusb://'.length), product: editIface.name || 'LoRa Radio' };
            _rnodeFound['android-usb'] = { state: 'done', devices: [_androidUsbSelectedDevice] };
        } else if (_rnodeIsTcpPort(port)) {
            _rnodeTcpDraft = _rnodeTcpInputValue(port);
        } else {
            _selectedSerialPort = { device: port, description: port };
            _rnodeFound.serial = { state: 'done', devices: [_selectedSerialPort] };
        }
    }

    var catalogReady = loadRnodePresetCatalog();
    var dlg = _rnodeDialog = RS.kit.dialog({
        title: editIface ? 'Edit radio' : 'Set up a radio',
        size: 'md',
        className: 'netdlg netdlg-radio',
        body: '<div class="netdlg-step" id="rnode-step-1"' + (editIface ? ' hidden' : '') + '>' + _rnodeStep1Html() + '</div>' +
            '<div class="netdlg-step" id="rnode-step-2"' + (editIface ? '' : ' hidden') + '>' + _rnodeStep2Html(editIface) + '</div>',
        foot: ' ',
        focus: false,
        onClose: function() { if (_rnodeDialog === dlg) _rnodeDialogClosed(); }
    });
    dlg.dialog.addEventListener('click', _rnodeDialogClick);
    dlg.dialog.addEventListener('input', _rnodeDialogInput);
    dlg.dialog.addEventListener('change', _rnodeDialogChange);
    dlg.dialog.addEventListener('keydown', _rnodeDialogKeydown);
    _rnodeRenderFoot();

    _rnodeApplyDefaultRadioControls();
    _rnodeSetAirtimeLimits(null, null);
    _rnodeSetInterfaceMode(_rnodeInitialInterfaceMode(editIface));
    _rnodeResetPublicMap();
    if (editIface) {
        var freq = _ifaceInt(editIface, 'frequency', 915000000);
        var bw = _ifaceInt(editIface, 'bandwidth', 250000);
        var sf = _ifaceInt(editIface, 'spreadingfactor', 9);
        var cr = _ifaceInt(editIface, 'codingrate', 5);
        var tx = _ifaceInt(editIface, 'txpower', 17);
        var airtimeShortRaw = parseFloat(_ifaceString(editIface, 'airtime_limit_short', ''));
        var airtimeLongRaw = parseFloat(_ifaceString(editIface, 'airtime_limit_long', ''));
        var airtimeShort = isNaN(airtimeShortRaw) ? null : airtimeShortRaw;
        var airtimeLong = isNaN(airtimeLongRaw) ? null : airtimeLongRaw;
        document.getElementById('rnode-iface-name').value = editIface.name || '';
        _rnodeLoadPublicMap(editIface);
        var applyRadioSelection = function() {
            if (_rnodeDialog !== dlg) return;
            var regionKey = _rnodeRegionForInterface(editIface, freq);
            var presetKey = _rnodePresetForInterface(editIface, bw, sf, cr, tx);
            _rnodeApplyRadioControls(
                regionKey,
                presetKey,
                freq,
                bw,
                sf,
                cr,
                tx,
                regionKey === _RNODE_CUSTOM_REGION_KEY || presetKey === _RNODE_CUSTOM_PRESET_KEY ||
                    airtimeShort !== null || airtimeLong !== null
            );
            _rnodeSetAirtimeLimits(airtimeShort, airtimeLong);
        };
        applyRadioSelection();
        catalogReady.then(applyRadioSelection).catch(function() {});
    } else {
        catalogReady.then(function() { if (_rnodeDialog === dlg && _rnodeStep === 1) _rnodeApplyDefaultRadioControls(); }).catch(function() {});
        _rnodeEnsureScan(mode);
    }
    _rnodeFocusStep();
}

function _rnodeDialogClosed() {
    _rnodeDialog = null;
    _bleSelectedDevice = null;
    _selectedSerialPort = null;
    _androidUsbSelectedDevice = null;
    _rnodeEditContext = null;
    _rnodePrefill = null;
}

function closeRnodeModal() {
    if (_rnodeDialog && _rnodeDialog.isOpen()) _rnodeDialog.close();
}

function _rnodeDialogClick(e) {
    var seg = e.target.closest('[data-netdlg-seg="rnode-type"] [role="radio"]');
    if (seg) {
        var type = seg.getAttribute('data-v');
        if (type !== _rnodeConnectionType) setRnodeConnectionType(type);
        return;
    }
    if (e.target.closest('#rnode-public-map-use-current')) {
        _rnodeSetPublicMapEnabled(true);
        _rnodeRequestPublicMapLocation();
        return;
    }
    var el = e.target.closest('[data-netdlg]');
    if (!el || el.disabled) return;
    switch (el.getAttribute('data-netdlg')) {
        case 'radio-pick': _rnodePick(parseInt(el.getAttribute('data-i'), 10)); return;
        case 'radio-scan': _rnodeScan(_rnodeConnectionType); return;
        case 'ble-settings': openIosBluetoothSettings(); return;
        case 'select': _netdlgSelectOpen(el); return;
        case 'disclose': _netdlgSetDisclosure(el.id, el.getAttribute('aria-expanded') !== 'true'); return;
        case 'back': rnodeWizardBack(); return;
        case 'submit':
            if (_rnodeStep === 1) rnodeWizardNext();
            else submitRnodeInterface();
            return;
    }
}

function _rnodeDialogInput(e) {
    var id = e.target.id;
    if (e.target.classList.contains('is-invalid')) _netdlgSetError(id, '');
    if (id === 'rnode-tcp-endpoint') {
        _rnodeTcpDraft = e.target.value;
        rnodeUpdateNextBtn();
    } else if (id === 'rnode-frequency') {
        _rnodeRefreshRegionFromFrequency();
    } else if (id === 'rnode-bandwidth' || id === 'rnode-spreading-factor' || id === 'rnode-coding-rate' || id === 'rnode-tx-power') {
        _rnodeRefreshPresetFromAdvanced();
    } else if (id === 'rnode-public-map-latitude' || id === 'rnode-public-map-longitude') {
        _rnodeSetPublicMapError('');
        _rnodeSetPublicMapStatus();
    }
}

function _rnodeDialogChange(e) {
    if (e.target.id !== 'rnode-public-map-enabled') return;
    if (e.target.checked) {
        e.target.checked = false;
        _rnodeEnablePublicMapWithWarning();
    } else {
        _rnodeSetPublicMapEnabled(false);
    }
}

function _rnodeDialogKeydown(e) {
    if (e.key !== 'Enter' || e.shiftKey || !e.target || e.target.tagName !== 'INPUT' || e.target.type === 'checkbox') return;
    e.preventDefault();
    if (_rnodeStep === 2) {
        submitRnodeInterface();
    } else {
        var next = _netdlgPrimary(_rnodeDialog);
        if (next && !next.disabled) rnodeWizardNext();
    }
}

function setRnodeConnectionType(type) {
    _rnodeConnectionType = type;
    _bleSelectedDevice = null;
    _selectedSerialPort = null;
    _androidUsbSelectedDevice = null;
    _rnodeRenderStep1();
    _rnodeEnsureScan(type);
}

function _rnodePick(index) {
    var type = _rnodeConnectionType;
    var found = _rnodeFound[type];
    var dev = found && found.devices[index];
    if (!dev) return;
    if (type === 'ble') _bleSelectedDevice = dev;
    else if (type === 'android-usb') _androidUsbSelectedDevice = dev;
    else _selectedSerialPort = dev;
    _rnodeRenderStep1();
}

function _rnodeDefaultInterfaceName(deviceName) {
    var value = String(deviceName || '').trim();
    var match = value.match(/^RNode[\s_-]*([0-9a-f]{4})$/i);
    if (match) return 'RNode_' + match[1].toUpperCase();
    return 'RNode';
}

function rnodeUpdateNextBtn() {
    var btn = _rnodeStep === 1 ? _netdlgPrimary(_rnodeDialog) : null;
    if (!btn) return;
    var hasDevice = false;
    if (_rnodeConnectionType === 'ble') {
        hasDevice = !!_bleSelectedDevice;
    } else if (_rnodeConnectionType === 'android-usb') {
        hasDevice = !!_androidUsbSelectedDevice;
    } else if (_rnodeConnectionType === 'tcp') {
        hasDevice = !!_rnodeTcpDraft.trim();
    } else {
        hasDevice = !!_selectedSerialPort;
    }
    btn.disabled = !hasDevice;
}

function rnodeWizardNext() {
    if (!_rnodeDialog || !_rnodeDialog.isOpen()) return;
    var nameInput = document.getElementById('rnode-iface-name');

    if (_rnodeConnectionType === 'ble' && _bleSelectedDevice) {
        if (!nameInput.value.trim()) {
            nameInput.value = _rnodeDefaultInterfaceName(_bleSelectedDevice.name);
        }
    } else if (_rnodeConnectionType === 'android-usb' && _androidUsbSelectedDevice) {
        if (!nameInput.value.trim()) nameInput.value = 'LoRa Radio';
    } else if (_rnodeConnectionType === 'tcp') {
        var tcpEndpoint = _normaliseRnodeTcpEndpoint(_rnodeTcpDraft);
        if (tcpEndpoint.error) {
            _netdlgFieldError('rnode-tcp-endpoint', tcpEndpoint.error);
            return;
        }
        _rnodeTcpDraft = tcpEndpoint.label;
        var tcpInput = document.getElementById('rnode-tcp-endpoint');
        if (tcpInput) tcpInput.value = tcpEndpoint.label;
        if (!nameInput.value.trim()) nameInput.value = 'LoRa Radio';
    } else if (_selectedSerialPort) {
        if (!nameInput.value.trim()) {
            // RNode_Firmware's advertised four-hex identifier is derived from
            // Bluetooth hardware state and is not exposed by the serial KISS
            // protocol. Do not substitute the unrelated EEPROM serial here.
            nameInput.value = 'RNode';
        }
    } else {
        return;
    }

    // A discovered radio's published settings, from "Use these radio settings".
    if (_rnodePrefill && !_rnodeEditContext) {
        var pre = _rnodePrefill;
        _rnodePrefill = null;
        _rnodePrefillName = pre.name || 'the discovered radio';
        var preTx = pre.tx || 17;
        _rnodeApplyRadioControls(
            _rnodeRegionForFrequency(pre.freq),
            _rnodePresetForParams(pre.bw, pre.sf, pre.cr, preTx),
            pre.freq, pre.bw, pre.sf, pre.cr, preTx, true
        );
    }
    var intro = document.getElementById('rnode-step-2-intro');
    if (intro) intro.textContent = 'Step 2 of 2 · Radio settings' + (_rnodePrefillName ? '. Filled in from ' + _rnodePrefillName + '.' : '.');

    // Bonding happens implicitly during add_lora_interface; OS pair dialog
    // pops mid-connect (Linux uses ble_rnode_passkey_prompt instead).
    _rnodeShowStep(2);
    _rnodeFocusStep();
}

// From a discovered radio: its published settings fill in step 2.
function openRnodeModalWithSettings(settings) {
    openRnodeModal('ble', null, settings || null);
}

function rnodeWizardBack() {
    _rnodeShowStep(1);
    _rnodeEnsureScan(_rnodeConnectionType);
    _rnodeFocusStep();
}

function refreshRnodeSerialPorts() {
    _rnodeFound.serial = { state: 'scanning', devices: [] };
    _rnodeRenderStep1();
    RS.invoke('api_serial_ports').then(function(ports) {
        ports = ports || [];
        _rnodeFound.serial = { state: 'done', devices: _rnodeKeepSelected('serial', ports) };
        _rnodeRenderStep1();
        if (ports.some(function(p) { return p.permission_denied; })) {
            showToast(
                'USB-Serial radio found, but permission was denied. Install Ratspeak\'s udev rule (with the AppImage, as /etc/udev/rules.d/70-ratspeak.rules, and delete an older 99-ratspeak.rules there) or join the dialout or uucp group, then reconnect it.',
                'toast-warning',
                8000
            );
        }
    }).catch(function() {
        _rnodeFound.serial = { state: 'error', devices: [], message: 'Couldn’t scan the USB ports. Try again.' };
        _rnodeRenderStep1();
    });
}

// Enumeration via Kotlin bridge so list populates before USB permission;
// Rust claims the device only after user picks it.
function refreshAndroidUsbDevices() {
    if (!hasAndroidBridge() || typeof window.RatspeakAndroid.listUsbDevices !== 'function') {
        _rnodeFound['android-usb'] = { state: 'error', devices: [], message: 'USB isn’t available on this device.' };
        _rnodeRenderStep1();
        return;
    }
    var devices = [];
    try {
        devices = JSON.parse(window.RatspeakAndroid.listUsbDevices() || '[]');
    } catch (e) {
        _rnodeFound['android-usb'] = { state: 'error', devices: [], message: 'Failed to enumerate USB devices: ' + String(e && e.message || e) };
        _rnodeRenderStep1();
        return;
    }
    _rnodeFound['android-usb'] = { state: 'done', devices: _rnodeKeepSelected('android-usb', devices) };
    _rnodeRenderStep1();
}

function scanBleDevices() {
    _rnodeFound.ble = { state: 'scanning', devices: [] };
    _rnodeRenderStep1();
    if (hasAndroidBridge()) {
        window._onBlePermissionResult = function(granted) {
            window._onBlePermissionResult = null;
            if (granted) {
                _doBleScan();
            } else {
                _rnodeFound.ble = { state: 'error', devices: [], message: 'Bluetooth permission denied. Grant permission in system settings to scan for LoRa radios.' };
                _rnodeRenderStep1();
            }
        };
        try {
            window.RatspeakAndroid.requestBlePermissions();
        } catch (e) {
            window._onBlePermissionResult = null;
            _rnodeFound.ble = { state: 'idle', devices: [] };
            _rnodeRenderStep1();
        }
        return;
    }
    _doBleScan();
}

function _bleScanFailed(message) {
    _rnodeFound.ble = { state: 'error', devices: [], message: message };
    _rnodeRenderStep1();
}

function _doBleScan() {
    _rnodeFound.ble = { state: 'scanning', devices: [] };
    _rnodeRenderStep1();
    // Watchdog for dropped callbacks / bridge crashes.
    var watchdog = setTimeout(function() {
        if (_rnodeFound.ble && _rnodeFound.ble.state === 'scanning') _bleScanFailed('Scan timed out. Try again.');
    }, 8000);
    var clearWatchdog = function() { clearTimeout(watchdog); };

    // btleplug fails on Android 14+; route through native Kotlin scanner.
    if (hasAndroidBridge() && typeof window.RatspeakAndroid.scanBleDevices === 'function') {
        window._onNativeBleScanResult = function(data) {
            window._onNativeBleScanResult = null;
            clearWatchdog();
            bleScanFinished(data);
        };
        try {
            window.RatspeakAndroid.scanBleDevices(5000);
        } catch (e) {
            window._onNativeBleScanResult = null;
            clearWatchdog();
            _bleScanFailed('Failed to start BLE scan.');
        }
        return;
    }

    // Desktop/iOS: invoke returns scan results synchronously.
    RS.invoke('scan_ble_devices').then(function(result) {
        clearWatchdog();
        renderBleDeviceList(result && Array.isArray(result.devices) ? result.devices : []);
    }).catch(function() {
        clearWatchdog();
        renderBleDeviceList([]);
    });
}

// Native scan results, and the runtime's ble_scan_results event.
function bleScanFinished(data) {
    if (!data || typeof data !== 'object') {
        if (_rnodeFound.ble && _rnodeFound.ble.state === 'scanning') renderBleDeviceList([]);
        return;
    }
    if (data.error) {
        _bleScanFailed(String(data.error));
        return;
    }
    renderBleDeviceList((data.devices || []).filter(function(d) {
        return d.device_type === 'rnode';
    }));
}

// Bond state is ground-truth on Android + Linux only; Apple/Windows always
// report bonded:false so the badge is hidden there.
//   TODO(ble-bonded-apple): retrievePeripheralsWithIdentifiers(_:) via objc2.
//   TODO(ble-bonded-windows): bind BluetoothLEDevice.…Pairing.IsPaired.
function _bondBadgeReliable() {
    if (hasAndroidBridge()) return true;
    var ua = (navigator.userAgent || '').toLowerCase();
    if (ua.indexOf('linux') >= 0 && ua.indexOf('android') < 0) return true;
    return false;
}

function renderBleDeviceList(devices) {
    var showBondBadge = _bondBadgeReliable();
    // Bonded-first only when the bond flag is meaningful; else RSSI-only.
    devices = (devices || []).slice().sort(function(a, b) {
        if (showBondBadge && a.bonded !== b.bonded) return b.bonded ? 1 : -1;
        return (b.rssi || -100) - (a.rssi || -100);
    });
    _rnodeFound.ble = { state: 'done', devices: _rnodeKeepSelected('ble', devices) };
    _rnodeRenderStep1();
}

var _rnodePresets = {};
var _rnodeRegions = {};
var _rnodeCatalogDefaults = { region: 'americas', preset: 'medium_fast' };
var _rnodeRadioLimits = {
    frequencyMin: 137000000,
    frequencyMax: 3000000000,
    bandwidthMin: 7800,
    bandwidthMax: 1625000,
    sfMin: 5,
    sfMax: 12,
    crMin: 5,
    crMax: 8,
    txMin: 0,
    txMax: 37,
};
var _rnodeCatalogPromise = null;

function _catalogArray(value) {
    if (Array.isArray(value)) return value;
    if (!value || typeof value !== 'object') return [];
    return Object.keys(value).map(function(key) {
        var entry = value[key] || {};
        entry.key = entry.key || key;
        return entry;
    });
}

function _rnodeShortMhz(hz) {
    return String(Math.round((parseInt(hz, 10) || 0) / 1000) / 1000);
}

// Region and Preset are kit Select fields; their menus carry each choice's band or parameters.
function _populateRnodeSelect(selectId, entries, defaultKey) {
    var previous = _netdlgSelectGet(selectId) || defaultKey || '';
    var choices = entries.map(function(entry) {
        var hint = '';
        if (selectId === 'rnode-region' && entry.min && entry.max) {
            hint = _rnodeShortMhz(entry.min) + '–' + _rnodeShortMhz(entry.max) + ' MHz';
        } else if (selectId === 'rnode-preset' && entry.bandwidth) {
            hint = 'SF' + entry.spreading_factor + ' · ' + _rnodeFormatKhz(entry.bandwidth) + ' kHz · CR ' + entry.coding_rate + ' · ' + entry.tx_power + ' dBm';
        }
        return { key: entry.key, label: entry.label || entry.key, hint: hint };
    });
    if (selectId === 'rnode-region') {
        choices.push({ key: _RNODE_CUSTOM_REGION_KEY, label: 'Custom frequency' });
    } else if (selectId === 'rnode-preset') {
        choices.push({ key: _RNODE_CUSTOM_PRESET_KEY, label: 'Custom radio parameters' });
    }
    _netdlgSelectOptions(selectId, choices);
    _netdlgSelectSet(selectId, choices.some(function(entry) { return entry.key === previous; })
        ? previous
        : (defaultKey || (choices[0] && choices[0].key) || ''));
}

function _applyRnodePresetCatalog(data) {
    var presets = _catalogArray(data && data.presets);
    var regions = _catalogArray(data && data.regions);
    var presetMap = {};
    var regionMap = {};
    presets.forEach(function(preset) {
        presetMap[preset.key] = {
            label: preset.label || preset.key,
            description: preset.description || '',
            sf: parseInt(preset.spreading_factor, 10),
            bw: parseInt(preset.bandwidth, 10),
            cr: parseInt(preset.coding_rate, 10),
            tx: parseInt(preset.tx_power, 10),
        };
    });
    regions.forEach(function(region) {
        regionMap[region.key] = {
            label: region.label || region.key,
            freq: parseInt(region.frequency || region.default, 10),
            min: parseInt(region.min, 10),
            max: parseInt(region.max, 10),
        };
    });
    if (presets.length > 0) _rnodePresets = presetMap;
    if (regions.length > 0) _rnodeRegions = regionMap;
    _rnodeCatalogDefaults = {
        region: (data && data.default_region) || _rnodeCatalogDefaults.region,
        preset: (data && data.default_preset) || _rnodeCatalogDefaults.preset,
    };
    _rnodeRadioLimits = {
        frequencyMin: parseInt(data && data.frequency_min, 10) || _rnodeRadioLimits.frequencyMin,
        frequencyMax: parseInt(data && data.frequency_max, 10) || _rnodeRadioLimits.frequencyMax,
        bandwidthMin: parseInt(data && data.bandwidth_min, 10) || _rnodeRadioLimits.bandwidthMin,
        bandwidthMax: parseInt(data && data.bandwidth_max, 10) || _rnodeRadioLimits.bandwidthMax,
        sfMin: parseInt(data && data.spreading_factor_min, 10) || _rnodeRadioLimits.sfMin,
        sfMax: parseInt(data && data.spreading_factor_max, 10) || _rnodeRadioLimits.sfMax,
        crMin: parseInt(data && data.coding_rate_min, 10) || _rnodeRadioLimits.crMin,
        crMax: parseInt(data && data.coding_rate_max, 10) || _rnodeRadioLimits.crMax,
        txMin: parseInt(data && data.tx_power_min, 10) || _rnodeRadioLimits.txMin,
        txMax: parseInt(data && data.tx_power_max, 10) || _rnodeRadioLimits.txMax,
    };
    _populateRnodeSelect('rnode-region', regions, _rnodeCatalogDefaults.region);
    _populateRnodeSelect('rnode-preset', presets, _rnodeCatalogDefaults.preset);
    _rnodeUpdateRadioHints();
}

function loadRnodePresetCatalog() {
    if (_rnodeCatalogPromise) return _rnodeCatalogPromise;
    _rnodeCatalogPromise = RS.invoke('api_rnode_presets')
        .then(function(data) {
            _applyRnodePresetCatalog(data || {});
            return data;
        })
        .catch(function(err) {
            _rnodeCatalogPromise = null;
            throw err;
        });
    return _rnodeCatalogPromise;
}

if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', function() {
        loadRnodePresetCatalog().catch(function() {});
    });
} else {
    loadRnodePresetCatalog().catch(function() {});
}

function submitRnodeInterface() {
    if (!_rnodeDialog || !_rnodeDialog.isOpen()) return;
    _netdlgClearErrors(_rnodeDialog);
    var name = document.getElementById('rnode-iface-name').value.trim();
    if (!name) {
        _netdlgFieldError('rnode-iface-name', 'Add a name.');
        return;
    }

    var port = '';
    if (_rnodeConnectionType === 'ble' && _bleSelectedDevice) {
        port = 'ble://' + _bleSelectedDevice.address;
    } else if (_rnodeConnectionType === 'android-usb' && _androidUsbSelectedDevice) {
        port = 'androidusb://' + _androidUsbSelectedDevice.device_name;
    } else if (_rnodeConnectionType === 'tcp') {
        var tcpEndpoint = _normaliseRnodeTcpEndpoint(_rnodeTcpDraft);
        if (tcpEndpoint.error) {
            _rnodeShowStep(1);
            _netdlgFieldError('rnode-tcp-endpoint', tcpEndpoint.error);
            return;
        }
        port = tcpEndpoint.port;
    } else if (_selectedSerialPort) {
        port = _selectedSerialPort.device;
    }
    if (!port) {
        _netdlgNotice(_rnodeDialog, 'No device selected. Go back and select a device.');
        return;
    }

    var radioSettings = _rnodeReadRadioSettings();
    if (radioSettings.error) {
        if (document.getElementById('rnode-advanced-body').contains(document.getElementById(radioSettings.field))) {
            _netdlgSetDisclosure('rnode-advanced', true);
        }
        _netdlgFieldError(radioSettings.field, radioSettings.error);
        return;
    }

    var publicMapSettings = null;
    if (_rnodeEditContext) {
        publicMapSettings = _rnodeReadPublicMapSettings();
        if (publicMapSettings.error) {
            _rnodeSetPublicMapError(publicMapSettings.error);
            return;
        }
        _rnodeSetPublicMapError('');
    }

    var submitBtn = _netdlgPrimary(_rnodeDialog);
    _netdlgBusy(submitBtn, _rnodeEditContext ? 'Saving…' : 'Connecting…');
    // Prompt for USB permission before Rust opens the device.
    var proceed = Promise.resolve(true);
    if (_rnodeConnectionType === 'android-usb' && _androidUsbSelectedDevice && hasAndroidBridge()) {
        var devName = _androidUsbSelectedDevice.device_name;
        var hasUsbPerm = false;
        try { hasUsbPerm = window.RatspeakAndroid.hasUsbPermission(devName); } catch (e) {}
        if (hasUsbPerm) {
            proceed = Promise.resolve(true);
        } else {
            proceed = new Promise(function(resolve) {
                window._onUsbPermissionResult = function(result) {
                    window._onUsbPermissionResult = null;
                    if (result && result.granted) { resolve(true); }
                    else {
                        var err = result && result.error ? result.error : 'USB permission is required to use this radio';
                        showToast(err, 'toast-warning', 4000);
                        resolve(false);
                    }
                };
                try { window.RatspeakAndroid.requestUsbPermission(devName); }
                catch (e) { window._onUsbPermissionResult = null; resolve(false); }
            });
        }
    }

    proceed.then(function(permOk) {
        if (!permOk) {
            _netdlgBusy(submitBtn, null);
            return;
        }
        var isEdit = !!_rnodeEditContext;
        var loraCommand = isEdit ? 'update_lora_interface' : 'add_lora_interface';
        var loraArgs = {
            name: name,
            port: port,
            mode: _rnodeReadInterfaceMode(),
            frequency: radioSettings.frequency,
            bandwidth: radioSettings.bandwidth,
            spreading_factor: radioSettings.spreadingFactor,
            coding_rate: radioSettings.codingRate,
            tx_power: radioSettings.txPower,
        };
        if (radioSettings.regionKey !== _RNODE_CUSTOM_REGION_KEY) loraArgs.region_key = radioSettings.regionKey;
        if (radioSettings.presetKey !== _RNODE_CUSTOM_PRESET_KEY) loraArgs.preset_key = radioSettings.presetKey;
        if (radioSettings.customParams) loraArgs.custom_params = true;
        if (radioSettings.airtimeShort !== null) loraArgs.airtime_limit_short = radioSettings.airtimeShort;
        if (radioSettings.airtimeLong !== null) loraArgs.airtime_limit_long = radioSettings.airtimeLong;
        if (isEdit) {
            loraArgs.old_name = _rnodeEditContext.oldName;
            loraArgs.public_map = publicMapSettings || { enabled: false };
        }
        var loraRequest = RS.invoke(loraCommand, { args: loraArgs });

        closeRnodeModal();
        if (_rnodeConnectionType === 'ble' && typeof rsProgress === 'function') {
            var bleName = name;
            window._activeProgressDialog = rsProgress({
                message: isEdit ? 'Restarting BLE LoRa radio...' : 'Connecting BLE LoRa radio...',
                operation: isEdit ? 'update_lora' : 'add_lora',
                onCancel: function() {
                    // Rust only rolls back config created by a fresh add; edits
                    // still need their active native/runtime connect cancelled.
                    RS.invoke('cancel_ble_connect', { name: bleName }).catch(function() {});
                    window._activeProgressDialog = null;
                },
            });
            // Native connect phases narrate only this dialog.
            window._activeProgressDialog.bleRnode = true;
            window._activeProgressDialog.cancelBleConnect = function() {
                RS.invoke('cancel_ble_connect', { name: bleName }).catch(function() {});
            };
        } else if (_rnodeConnectionType === 'android-usb' && typeof rsProgress === 'function') {
            window._activeProgressDialog = rsProgress({
                message: isEdit ? 'Restarting USB LoRa radio...' : 'Connecting USB LoRa radio...',
                operation: isEdit ? 'update_lora' : 'add_lora',
            });
        } else if (_rnodeConnectionType === 'tcp' && typeof rsProgress === 'function') {
            window._activeProgressDialog = rsProgress({
                message: isEdit ? 'Restarting TCP LoRa radio...' : 'Connecting TCP LoRa radio...',
                operation: isEdit ? 'update_lora' : 'add_lora',
            });
        }
        loraRequest.catch(function(err) {
            if (window._activeProgressDialog && window._activeProgressDialog.error) {
                window._activeProgressDialog.error((err && err.message) || 'Failed to configure LoRa interface');
            } else {
                showToast((err && err.message) || 'Could not configure LoRa interface', 'toast-error', 8000);
            }
        });
    });
}

function _normaliseConnectEditContext(editContext) {
    if (!editContext || typeof editContext !== 'object') return null;
    var ifaceType = editContext.ifaceType;
    var oldName = typeof editContext.oldName === 'string' ? editContext.oldName.trim() : '';
    if ((ifaceType !== 'tcp_client' && ifaceType !== 'backbone_client') || !oldName) return null;
    return {
        ifaceType: ifaceType,
        oldName: oldName,
        iface: editContext.iface || {},
    };
}

function _normaliseHostEditContext(editContext, ifaceType) {
    if (!editContext || typeof editContext !== 'object') return null;
    var oldName = typeof editContext.oldName === 'string' ? editContext.oldName.trim() : '';
    if (editContext.ifaceType !== ifaceType || !oldName) return null;
    return {
        ifaceType: ifaceType,
        oldName: oldName,
        iface: editContext.iface || {},
    };
}

// ------------------------------------------------------------------ public servers (joined on the Network screen)
// A public server is reached by a TCP client or a Backbone client alike. Of
// several saved interfaces to one server, one that is online is the match.
function _connectMatchingPublicInterface(server, ifaces) {
    ifaces = ifaces || window._hubInterfacesData || window._cachedConfigIfaces || {};
    var match = null;
    ['tcp_client', 'backbone_client'].forEach(function(ifaceType) {
        var clients = Array.isArray(ifaces[ifaceType]) ? ifaces[ifaceType] : [];
        for (var i = 0; i < clients.length; i++) {
            var iface = clients[i] || {};
            if (!_publicServerMatchesEndpoint(server, iface.target_host, iface.target_port)) continue;
            var live = (typeof getInterfaceLiveStatus === 'function') ? getInterfaceLiveStatus(iface.name || '') : null;
            var online = live ? live.online !== false : false;
            if (!match || (online && !match.online)) {
                match = { iface: iface, ifaceType: ifaceType, online: online };
            }
        }
    });
    return match;
}

function _publicServerState(server, ifaces) {
    var match = _connectMatchingPublicInterface(server, ifaces);
    var connected = !!(match && match.online);
    return {
        match: match,
        connected: connected,
        pending: _connectPendingPublicServerKey === _tcpServerKey(server.host, server.port) && !connected,
        configured: !!match && !connected
    };
}

function _redrawPublicServers() {
    if (RS.networkView && typeof RS.networkView.render === 'function') RS.networkView.render(true);
}

// A public server is always joined as a TCP client, which Backbone servers accept too.
function connectPublicServer(server) {
    _connectPendingPublicServerKey = _tcpServerKey(server.host, server.port);
    _redrawPublicServers();
    RS.invoke('add_tcp_connection', {
        args: { host: server.host, port: server.port, name: server.name, ifac_enabled: false, ifac_network_name: '', ifac_passphrase: '' }
    }).catch(function(err) {
        clearConnectPublicPending();
        showToast((err && err.message) || 'Could not join ' + server.name, 'toast-error', 8000);
    });
}

function clearConnectPublicPending() {
    _connectPendingPublicServerKey = null;
    _redrawPublicServers();
}

function resumePublicServerInterface(server, match) {
    var iface = match && match.iface ? match.iface : null;
    if (!iface || !iface.name) {
        connectPublicServer(server);
        return;
    }

    _connectPendingPublicServerKey = _tcpServerKey(server.host, server.port);
    _redrawPublicServers();
    RS.invoke('resume_interface', {
        args: {
            name: iface.name,
            iface_type: match.ifaceType || 'tcp_client'
        }
    }).then(function() {
        if (typeof refreshConfigInterfaces === 'function') refreshConfigInterfaces();
    }).catch(function(err) {
        clearConnectPublicPending();
        showToast((err && err.message) || 'Could not reconnect', 'toast-error', 8000);
    });
}

// Applied interfaces already redraw the Network screen; a forced refresh reads them again first.
function refreshConnectPublicServers(ifaces, opts) {
    opts = opts || {};
    if (ifaces && !opts.force) return;
    if (!opts.force && (window._hubInterfacesData || window._cachedConfigIfaces)) {
        _redrawPublicServers();
        return;
    }
    RS.invoke('api_hub_interfaces').then(function(data) {
        window._hubInterfacesData = data || {};
        _redrawPublicServers();
    }).catch(_redrawPublicServers);
}

// ------------------------------------------------------------------ network name and passphrase (IFAC)
function _ifaceIfacNetworkName(iface) {
    return _ifaceString(iface, 'network_name', _ifaceString(iface, 'networkname', ''));
}

function _ifaceIfacPassphrase(iface) {
    return _ifaceString(iface, 'passphrase', _ifaceString(iface, 'pass_phrase', ''));
}

function _ifaceHasIfac(iface) {
    return !!(_ifaceIfacNetworkName(iface) || _ifaceIfacPassphrase(iface));
}

// The fields under the "Network name and passphrase" switch (<prefix>-ifac). Only the size
// override stays behind developer mode; the runtime keeps a stored size the form leaves out.
function _ifacFieldsHtml(prefix, iface, on, passphraseHint, twoUp) {
    var fields = _netdlgTextField(prefix + '-ifac-network-name', 'Network name', {
        value: iface ? _ifaceIfacNetworkName(iface) : '', placeholder: 'Optional', max: 128
    }) + _netdlgTextField(prefix + '-ifac-passphrase', 'Passphrase', {
        type: 'password', value: iface ? _ifaceIfacPassphrase(iface) : '', placeholder: passphraseHint, max: 256
    });
    return '<div class="ui-prop settings-prop-stack" id="' + prefix + '-ifac-fields"' + (on ? '' : ' hidden') + '>' +
        (twoUp ? '<div class="netdlg-two">' + fields + '</div>' : fields) +
        _netdlgTextField(prefix + '-ifac-size', 'IFAC size', {
            value: iface ? _ifaceString(iface, 'ifac_size', '') : '', placeholder: '16', mode: 'numeric', max: 2, unit: 'bytes',
            attrs: ' data-netdlg-dev' + (_developerModeEnabled() ? '' : ' hidden')
        }) +
    '</div>';
}

function _ifacSyncFields(prefix) {
    var toggle = document.getElementById(prefix + '-ifac');
    var fields = document.getElementById(prefix + '-ifac-fields');
    if (fields) fields.hidden = !(toggle && toggle.checked);
}

function _ifacReadValues(prefix) {
    var useIfac = !!(document.getElementById(prefix + '-ifac') || {}).checked;
    var values = {
        ifac_enabled: useIfac,
        ifac_network_name: useIfac ? _netdlgValue(prefix + '-ifac-network-name') : '',
        ifac_passphrase: useIfac ? _netdlgValue(prefix + '-ifac-passphrase') : ''
    };
    var sizeText = _netdlgValue(prefix + '-ifac-size');
    var size = /^\d+$/.test(sizeText) ? Number(sizeText) : NaN;
    if (useIfac && size >= 1 && size <= 64) values.ifac_size = size;
    return values;
}

function _ifacApplyArgs(prefix, args) {
    var v = _ifacReadValues(prefix);
    args.ifac_enabled = v.ifac_enabled;
    args.ifac_network_name = v.ifac_network_name;
    args.ifac_passphrase = v.ifac_passphrase;
    if (v.ifac_size !== undefined) args.ifac_size = v.ifac_size;
    return args;
}

// The switch on with neither field filled is always a mistake; the submit stops at the field.
function _ifacGuardEmpty(prefix) {
    var v = _ifacReadValues(prefix);
    if (v.ifac_enabled && _netdlgValue(prefix + '-ifac-size') && v.ifac_size === undefined) {
        return _netdlgFieldError(prefix + '-ifac-size', 'Use a size from 1 to 64 bytes.');
    }
    if (v.ifac_enabled && !v.ifac_network_name && !v.ifac_passphrase) {
        return _netdlgFieldError(prefix + '-ifac-network-name', 'Add a network name or passphrase.');
    }
    return true;
}

// ------------------------------------------------------------------ Add your own server
var _connectEditContext = null;
var _connectDialog = null;
// While the dialog waits for its operation: { edit }.
var _connectAwaiting = null;
var _connectTimeout = null;

function openConnectModal(editContext) {
    if (_connectDialog && _connectDialog.isOpen()) _connectDialog.close();
    _connectEditContext = _normaliseConnectEditContext(editContext);
    _connectAwaiting = null;
    var iface = _connectEditContext ? _connectEditContext.iface : null;
    var isEdit = !!_connectEditContext;
    var isBackboneEdit = isEdit && _connectEditContext.ifaceType === 'backbone_client';
    var ifac = _ifaceHasIfac(iface);
    var dlg = _connectDialog = RS.kit.dialog({
        title: isEdit ? 'Edit server' : 'Add your own server',
        size: 'md',
        className: 'netdlg netdlg-server',
        body: '<p>Connect to a server someone gave you, or one you run.</p>' +
            _netdlgTextField('connect-host', 'Address', { value: iface ? _ifaceString(iface, 'target_host', '') : '', placeholder: 'rns.example.org', mode: 'url', max: 253 }) +
            _netdlgTextField('connect-port', 'Port', { value: iface ? _ifaceString(iface, 'target_port', '') : '', placeholder: '4242', mode: 'numeric', max: 5 }) +
            _netdlgTextField('connect-name', 'Name', { value: iface ? _ifaceString(iface, 'name', '') : '', placeholder: 'Shown in Connectivity', max: 32, help: 'Optional.' }) +
            '<div class="ui-group">' +
                _netdlgSwitch('connect-ifac', 'Network name and passphrase', 'Use the ones the server’s operator gave you.', ifac) +
                _ifacFieldsHtml('connect', iface, ifac, 'From the server’s operator') +
                (_netdlgBackboneAvailable() || isBackboneEdit
                    ? _netdlgSwitch('connect-backbone', 'Use Backbone', 'Experimental. Faster for always-on servers.', isBackboneEdit, { disabled: isEdit })
                    : '') +
            '</div>' + _netdlgNoticeSlot(),
        foot: _netdlgFoot(isEdit ? 'Save' : 'Connect'),
        onClose: function() {
            if (_connectDialog !== dlg) return;
            _connectDialog = null;
            _connectEditContext = null;
            _connectAwaiting = null;
            _clearConnectTimeout();
        }
    });
    dlg.dialog.addEventListener('click', function(e) {
        var el = e.target.closest('[data-netdlg="submit"]');
        if (el && !el.disabled) submitConnection();
    });
    dlg.dialog.addEventListener('change', function(e) {
        if (e.target.id === 'connect-ifac') _ifacSyncFields('connect');
    });
    dlg.dialog.addEventListener('input', function(e) {
        if (e.target.classList.contains('is-invalid')) _netdlgSetError(e.target.id, '');
    });
    dlg.dialog.addEventListener('keydown', function(e) {
        if (e.key !== 'Enter' || e.shiftKey || e.target.tagName !== 'INPUT' || e.target.type === 'checkbox') return;
        e.preventDefault();
        submitConnection();
    });
}

function _clearConnectTimeout() {
    if (_connectTimeout) {
        clearTimeout(_connectTimeout);
        _connectTimeout = null;
    }
}

function _connectFailed(message) {
    _clearConnectTimeout();
    _connectAwaiting = null;
    if (!_connectDialog || !_connectDialog.isOpen()) {
        showToast(message, 'toast-error', 8000);
        return;
    }
    _netdlgBusy(_netdlgPrimary(_connectDialog), null);
    _netdlgNotice(_connectDialog, message);
}

function _handleConnectInvokeError(err) {
    clearConnectPublicPending();
    _connectFailed(err && err.message ? err.message : 'Connection request failed');
}

function submitConnection() {
    var dlg = _connectDialog;
    if (!dlg || !dlg.isOpen() || _connectAwaiting) return;
    _netdlgClearErrors(dlg);
    var host = _netdlgValue('connect-host');
    if (!host || /[\s\/\\]/.test(host)) {
        _netdlgFieldError('connect-host', 'Enter a full address, like rns.example.org.');
        return;
    }
    var port = _netdlgPort('connect-port', 4242);
    if (port === null) {
        _netdlgFieldError('connect-port', 'Use a port from 1 to 65535.');
        return;
    }
    if (!_ifacGuardEmpty('connect')) return;
    var name = _netdlgValue('connect-name');

    var editContext = _normaliseConnectEditContext(_connectEditContext);
    // Backbone routes to add_backbone_connection (desktop only).
    var useBackbone = !!(document.getElementById('connect-backbone') || {}).checked;
    var command, args;
    if (editContext && editContext.ifaceType === 'backbone_client') {
        var bb = editContext.iface || {};
        command = 'update_backbone_connection';
        args = {
            old_name: editContext.oldName,
            host: host,
            port: port,
            name: name || ('Backbone to ' + host + ':' + port),
            prefer_ipv6: _ifaceBool(bb, 'prefer_ipv6'),
            connect_timeout: _ifaceInt(bb, 'connect_timeout', null),
            max_reconnect_tries: _ifaceInt(bb, 'max_reconnect_tries', null),
            i2p_tunneled: _ifaceBool(bb, 'i2p_tunneled'),
        };
    } else if (editContext) {
        command = 'update_tcp_connection';
        args = { old_name: editContext.oldName, host: host, port: port, name: name || (host + ':' + port) };
    } else if (useBackbone) {
        command = 'add_backbone_connection';
        args = { host: host, port: port, name: name || ('Backbone to ' + host + ':' + port) };
    } else {
        command = 'add_tcp_connection';
        args = { host: host, port: port, name: name || (host + ':' + port) };
    }

    _connectAwaiting = { edit: !!editContext };
    _netdlgBusy(_netdlgPrimary(dlg), editContext ? 'Saving…' : 'Connecting…');
    // The backend acks with an operation status; recover if it never does.
    _clearConnectTimeout();
    _connectTimeout = setTimeout(function() {
        _connectTimeout = null;
        if (_connectAwaiting) _connectFailed('No answer from the network yet. Try again.');
    }, 30000);
    RS.invoke(command, { args: _ifacApplyArgs('connect', args) }).catch(_handleConnectInvokeError);
}

// ------------------------------------------------------------------ Host setup
// Standard (TCP) or Backbone (desktop only) in one dialog, with the Private networks discovery switches.
var _hostEditContext = null;
var _hostDialog = null;
// While the dialog waits for its operation: { edit }.
var _hostAwaiting = null;
// { type: 'tcp' | 'backbone', ifac, disc, share, priv, reachable, published }.
var _hostForm = null;

function openHostModal(editContext) {
    _openHostDialog('tcp', _normaliseHostEditContext(editContext, 'tcp_server'));
}

function openBackboneHostModal(editContext) {
    _openHostDialog('backbone', _normaliseHostEditContext(editContext, 'backbone_server'));
}

function _hostTypeHelp(type) {
    return type === 'backbone' ? 'For always-on transport nodes, not mobile networks.' : 'Works for most people.';
}

function _hostDiscoveryHtml() {
    if (!_hostForm || !RS.hostDiscovery) return '';
    return RS.hostDiscovery.html({
        ifac: _hostForm.ifac, disc: _hostForm.disc, share: _hostForm.share, priv: _hostForm.priv, reachable: _hostForm.reachable
    }, 'host-');
}

function _hostRenderDiscovery() {
    if (!_hostDialog || !_hostDialog.isOpen()) return;
    var reach = document.getElementById('host-reachable');
    if (reach) _hostForm.reachable = reach.value;
    _netdlgRerender(document.getElementById('host-discovery'), _hostDiscoveryHtml());
}

function _openHostDialog(type, editContext) {
    if (_hostDialog && _hostDialog.isOpen()) _hostDialog.close();
    _netdlgBindIdentity();
    _hostEditContext = editContext;
    _hostAwaiting = null;
    var iface = editContext ? editContext.iface : null;
    var saved = RS.hostDiscovery ? RS.hostDiscovery.state(iface) : { ifac: false, disc: false, share: false, priv: false, reachable: '' };
    _hostForm = {
        type: type,
        ifac: saved.ifac || _ifaceHasIfac(iface),
        disc: saved.disc,
        share: saved.share,
        priv: saved.priv,
        reachable: saved.reachable,
        // A save of a server that is or was published restarts the network.
        published: !!(editContext && saved.disc)
    };
    var isEdit = !!editContext;
    var backbone = type === 'backbone';
    var listen = !iface ? '' : (backbone ? (_ifaceString(iface, 'listen_on', '') || _ifaceString(iface, 'listen_ip', '')) : _ifaceString(iface, 'listen_ip', ''));
    var dlg = _hostDialog = RS.kit.dialog({
        title: isEdit ? 'Edit host' : 'Host a server',
        size: 'md',
        className: 'netdlg netdlg-host',
        body: '<p>Let other Reticulum devices connect to this computer. Keep Ratspeak open while you host.</p>' +
            (_netdlgBackboneAvailable()
                ? '<div class="ui-field"><span class="ui-label" id="host-type-label">Server type</span>' +
                    _netdlgSeg('host-type', [['tcp', 'Standard'], ['backbone', 'Backbone']], type, 'Server type', isEdit) +
                    '<p class="ui-help" id="host-type-help">' + _hostTypeHelp(type) + '</p></div>'
                : '') +
            '<div class="netdlg-two">' +
                _netdlgTextField('host-port', 'Port', { value: iface ? _ifaceString(iface, 'listen_port', '') : '', placeholder: '4242', mode: 'numeric', max: 5 }) +
                _netdlgTextField('host-listen-ip', 'Listen on', { value: listen, placeholder: '0.0.0.0', mono: true, max: 64, help: 'All adapters if empty.' }) +
            '</div>' +
            _netdlgTextField('host-name', 'Name', { value: iface ? _ifaceString(iface, 'name', '') : '', placeholder: backbone ? 'Backbone server' : 'My server', max: 32, help: 'Optional.' }) +
            '<div class="ui-group">' +
                _netdlgSwitch('host-ifac', 'Network name and passphrase', 'Devices need both to connect.', _hostForm.ifac) +
                _ifacFieldsHtml('host', iface, _hostForm.ifac, 'Devices enter this to connect', true) +
            '</div>' +
            '<section class="netdlg-section" aria-labelledby="host-discovery-title"><h3 class="ui-section-title" id="host-discovery-title">Discovery</h3>' +
                '<div id="host-discovery">' + _hostDiscoveryHtml() + '</div></section>' +
            '<div class="ui-notice ui-notice--info">' + _netdlgIcon('info') + '<div class="ui-notice-body">Your firewall or router may need to allow this port.</div></div>' +
            _netdlgNoticeSlot(),
        foot: _netdlgFoot(isEdit ? 'Save' : 'Start hosting'),
        onClose: function() {
            if (_hostDialog !== dlg) return;
            _hostDialog = null;
            _hostEditContext = null;
            _hostAwaiting = null;
            _hostForm = null;
        }
    });
    dlg.dialog.addEventListener('click', _hostDialogClick);
    dlg.dialog.addEventListener('change', _hostDialogChange);
    dlg.dialog.addEventListener('input', function(e) {
        if (e.target.classList.contains('is-invalid')) _netdlgSetError(e.target.id, '');
        if (e.target.id === 'host-reachable') _hostForm.reachable = e.target.value;
    });
    dlg.dialog.addEventListener('keydown', function(e) {
        if (e.key !== 'Enter' || e.shiftKey || e.target.tagName !== 'INPUT' || e.target.type === 'checkbox') return;
        e.preventDefault();
        submitHostServer();
    });
}

function _hostDialogClick(e) {
    if (e.target.closest('#host-discovery') && RS.hostDiscovery && RS.hostDiscovery.handleInline(e)) return;
    var seg = e.target.closest('[data-netdlg-seg="host-type"] [role="radio"]');
    if (seg) {
        if (seg.disabled) return;
        _hostForm.type = seg.getAttribute('data-v');
        _netdlgSegSet(_hostDialog.dialog, 'host-type', _hostForm.type);
        document.getElementById('host-type-help').textContent = _hostTypeHelp(_hostForm.type);
        document.getElementById('host-name').placeholder = _hostForm.type === 'backbone' ? 'Backbone server' : 'My server';
        return;
    }
    var submit = e.target.closest('[data-netdlg="submit"]');
    if (submit && !submit.disabled) submitHostServer();
}

// Share access needs the network name and passphrase, so the switches redraw on every change.
function _hostDialogChange(e) {
    var key = e.target.getAttribute('data-hd');
    if (e.target.id === 'host-ifac') {
        _hostForm.ifac = e.target.checked;
        if (!_hostForm.ifac) _hostForm.share = false;
        _ifacSyncFields('host');
        _hostRenderDiscovery();
    } else if (key === 'disc' || key === 'share' || key === 'priv') {
        _hostForm[key] = e.target.checked;
        _hostRenderDiscovery();
    }
}

// The runtime's refusals land on their field when it shows, otherwise in the dialog's notice.
function _hostShowError(message) {
    message = message || 'The server couldn’t start. Try again.';
    var field = /address others/i.test(message) ? 'host-reachable'
        : (/network name or passphrase/i.test(message) ? 'host-ifac-network-name' : '');
    var input = field ? document.getElementById(field) : null;
    if (input && input.offsetParent !== null) _netdlgFieldError(field, message);
    else _netdlgNotice(_hostDialog, message);
}

function submitHostServer() {
    var dlg = _hostDialog;
    if (!dlg || !dlg.isOpen() || _hostAwaiting) return;
    _netdlgClearErrors(dlg);
    var backbone = _hostForm.type === 'backbone';
    var editContext = _normaliseHostEditContext(_hostEditContext, backbone ? 'backbone_server' : 'tcp_server');
    var port = _netdlgPort('host-port', 4242);
    if (port === null) {
        _netdlgFieldError('host-port', 'Use a port from 1 to 65535.');
        return;
    }
    if (!_ifacGuardEmpty('host')) return;

    // Only what the switches show is sent; a hidden setting keeps its saved value.
    var group = document.getElementById('host-discovery');
    var discovery = group && RS.hostDiscovery ? RS.hostDiscovery.read(group) : {};
    var shown = { reachable_on: 'reachable', publish_ifac: 'share', discovery_encrypt: 'priv' };
    Object.keys(shown).forEach(function(key) {
        if (!group || !group.querySelector('[data-hd="' + shown[key] + '"]')) delete discovery[key];
    });
    if (discovery.discoverable && !discovery.reachable_on) {
        _netdlgFieldError('host-reachable', 'Add the address others connect to.');
        return;
    }
    var identity = RS.networkIdentity && RS.networkIdentity.state();
    if (discovery.discoverable && discovery.discovery_encrypt && identity && !identity.exists) {
        _netdlgNotice(dlg, 'Create or import a network identity first.');
        return;
    }

    var args = _ifacApplyArgs('host', {
        listen_port: port,
        listen_ip: _netdlgValue('host-listen-ip') || '0.0.0.0',
        name: _netdlgValue('host-name') || ((backbone ? 'Backbone Server :' : 'TCP Server :') + port),
    });
    Object.keys(discovery).forEach(function(key) { args[key] = discovery[key]; });
    if (editContext) {
        args.old_name = editContext.oldName;
        if (backbone) {
            args.prefer_ipv6 = _ifaceBool(editContext.iface, 'prefer_ipv6');
            args.device = _ifaceString(editContext.iface, 'device', '') || null;
        }
    }
    var command = backbone
        ? (editContext ? 'update_backbone_server' : 'add_backbone_server')
        : (editContext ? 'update_tcp_server' : 'add_tcp_server');
    var submitBtn = _netdlgPrimary(dlg);
    var run = function() { return RS.invoke(command, { args: args }); };

    // A published server's save restarts the network, and the app confirms restarts one way.
    var ns = RS.networkSettings;
    if ((discovery.discoverable || _hostForm.published) && ns && typeof ns.commitRestart === 'function') {
        if (ns.busy) {
            _netdlgNotice(dlg, 'The network is restarting. Try again in a moment.');
            return;
        }
        _netdlgBusy(submitBtn, editContext ? 'Saving…' : 'Starting…');
        // A refusal shows once: in this dialog, or as a toast after it has closed.
        var refused = function(err) {
            var message = (err && err.message) || 'The server couldn’t start. Try again.';
            if (_hostDialog === dlg) _hostShowError(message);
            else showToast(message, 'toast-error', 8000);
        };
        ns.commitRestart({ run: run, done: 'Network restarted', error: refused }).then(function(result) {
            if (result) dlg.close();
            else _netdlgBusy(submitBtn, null);
        });
        return;
    }

    _hostAwaiting = { edit: !!editContext };
    _netdlgBusy(submitBtn, editContext ? 'Saving…' : 'Starting…');
    run().then(function(result) {
        if (!result || !result.restarted) return;
        _hostAwaiting = null;
        showToast('Network restarted', 'toast-success', 3000);
        dlg.close();
    }, function(err) {
        _hostAwaiting = null;
        _netdlgBusy(submitBtn, null);
        _hostShowError(err && err.message);
    });
}

// Operation status for the dialog that waits on it. Returns 'added' for a server added from Add
// your own server, 'handled' when a dialog showed the result, and false otherwise.
function connectionDialogOperation(data) {
    if (!data || !data.done) return false;
    var op = data.operation;
    if (op === 'add_tcp' || op === 'add_backbone' || op === 'update_tcp' || op === 'update_backbone') {
        if (data.error) clearConnectPublicPending();
        if (!_connectAwaiting) return false;
        var adding = !_connectAwaiting.edit;
        if (data.error) {
            _connectFailed(_netdlgOpError(data));
            return 'handled';
        }
        _clearConnectTimeout();
        _connectAwaiting = null;
        if (_connectDialog) _connectDialog.close();
        return adding ? 'added' : 'handled';
    }
    if (op === 'add_server' || op === 'update_server' || op === 'add_backbone_server' || op === 'update_backbone_server') {
        if (!_hostAwaiting) return false;
        _hostAwaiting = null;
        if (data.error) {
            _netdlgBusy(_netdlgPrimary(_hostDialog), null);
            _hostShowError(_netdlgOpError(data));
        } else if (_hostDialog) {
            _hostDialog.close();
        }
        return 'handled';
    }
    return false;
}

// A legacy value has no reliable producer marker. Only an explicit choice may
// convert it; opening settings never changes the file.
function resolveInterfaceIfacUnits(ifaceType, ifaceName, iface) {
    var migration = iface.ifac_migration;
    if (!migration) {
        showPreConditionToast(iface.ifac_error || 'Correct the IFAC settings before editing this interface');
        return;
    }
    var choices = [{ label: 'Keep existing Ratspeak size (' + migration.bytes + ' bytes)', value: 'bytes' }];
    if (migration.python_bytes !== null && migration.python_bytes !== undefined) {
        choices.push({ label: 'Use Python bit size (' + migration.python_bytes + ' bytes)', value: 'bits' });
    }
    rsChoice({
        title: 'Confirm IFAC size',
        message: 'This saved interface does not say which units its IFAC size uses. Choose the size used by the other peer. A backup will be kept.',
        choices: choices
    }).then(function(units) {
        if (units === null) return;
        return RS.invoke('migrate_interface_ifac_units', { args: {
            name: ifaceName, source_units: units, expected_source_hash: migration.source_hash
        }}).then(function() {
            loadHubInterfaces();
            showToast('IFAC size saved. Use Resume to connect this interface.', 'toast-success');
        }).catch(function(error) {
            showToast((error && error.message) || 'Could not update IFAC settings', 'toast-error', 8000);
        });
    });
}

function openInterfaceEditModal(ifaceType, ifaceName, iface) {
    if (iface && iface.ifac_error) {
        resolveInterfaceIfacUnits(ifaceType, ifaceName, iface);
        return;
    }
    iface = iface || {};
    var context = { ifaceType: ifaceType, oldName: ifaceName, iface: iface };
    if (ifaceType === 'rnode') {
        openRnodeModal(_rnodeModeForPort(_ifaceString(iface, 'port', '')), iface);
    } else if (ifaceType === 'tcp_client' || ifaceType === 'backbone_client') {
        openConnectModal(context);
    } else if (ifaceType === 'tcp_server') {
        openHostModal(context);
    } else if (ifaceType === 'backbone_server') {
        openBackboneHostModal(context);
    }
}

// ------------------------------------------------------------------ Local Network options
// The name up front; the network name and reach behind Advanced, plus ports, address type,
// adapters, and bitrate on a wide layout (the Python rnsd knobs).
var _localDialog = null;
var _localAdaptersLoaded = false;

_netdlgSelectOptions('local-reach', [
    { key: 'link', label: 'This Wi-Fi or wired network', hint: 'Link scope' },
    { key: 'admin', label: 'Administrative boundary', hint: 'Admin scope' },
    { key: 'site', label: 'Site', hint: 'IPv6 site scope' },
    { key: 'organisation', label: 'Organisation', hint: 'IPv6 organisation scope' },
    { key: 'global', label: 'Global', hint: 'IPv6 global scope' }
]);
_netdlgSelectOptions('local-address-type', [
    { key: 'temporary', label: 'Temporary', hint: 'RFC 4291 transient address' },
    { key: 'permanent', label: 'Permanent', hint: 'Sets the multicast flag' }
]);

function _localList(value) {
    if (Array.isArray(value)) return value.map(String);
    return String(value || '').split(',').map(function(item) { return item.trim(); }).filter(Boolean);
}

function _localCurrent() {
    var data = window._hubInterfacesData || window._cachedConfigIfaces || {};
    return (Array.isArray(data.auto) && data.auto[0]) || null;
}

function _localValues(cur) {
    var bitrate = parseFloat(_ifaceString(cur, 'configured_bitrate', ''));
    return {
        name: (cur && cur.name) || 'Local Network',
        group: _ifaceString(cur, 'group_id', 'reticulum'),
        scope: _ifaceString(cur, 'discovery_scope', 'link').toLowerCase(),
        addr: _ifaceString(cur, 'multicast_address_type', 'temporary').toLowerCase(),
        dport: _ifaceString(cur, 'discovery_port', '29716'),
        aport: _ifaceString(cur, 'data_port', '42671'),
        devices: cur ? _localList(cur.devices) : [],
        ignored: cur ? _localList(cur.ignored_devices) : [],
        bitrate: isNaN(bitrate) ? '' : String(bitrate / 1000000)
    };
}

// enable_auto_interface's options: only what differs from the defaults.
function _localOptions(v) {
    var options = {};
    if (v.group && v.group !== 'reticulum') options.group_id = v.group;
    if (v.scope && v.scope !== 'link') options.discovery_scope = v.scope;
    if (v.addr && v.addr !== 'temporary') options.multicast_address_type = v.addr;
    var dp = parseInt(v.dport, 10);
    var ap = parseInt(v.aport, 10);
    if (!isNaN(dp) && dp !== 29716) options.discovery_port = dp;
    if (!isNaN(ap) && ap !== 42671) options.data_port = ap;
    if (v.devices.length) options.devices = v.devices.slice().sort();
    if (v.ignored.length) options.ignored_devices = v.ignored.slice();
    var br = parseFloat(v.bitrate);
    if (!isNaN(br) && br > 0) options.configured_bitrate = Math.round(br * 1000000);
    return options;
}

function _localAdapterRow(name, label, checked) {
    return '<label class="ui-check-row"><input type="checkbox" value="' + _netdlgEsc(name) + '" data-local-adapter' + (checked ? ' checked' : '') + '>' +
        '<span class="ui-check">' + _netdlgIcon('check') + '</span><span>' + _netdlgEsc(label) + '</span></label>';
}

function _localLoadAdapters(chosen) {
    _localAdaptersLoaded = false;
    RS.invoke('api_list_network_interfaces', {}).then(function(resp) {
        var box = document.getElementById('local-adapters');
        if (!box) return;
        var names = {};
        var rows = ((resp && resp.interfaces) || []).filter(function(iface) { return !iface.is_loopback; }).map(function(iface) {
            names[iface.name] = true;
            return _localAdapterRow(iface.name, iface.name + ' — ' + (iface.addr_v6_link_local || iface.addr_v4 || 'no address'), chosen.indexOf(iface.name) >= 0);
        });
        // A chosen adapter that isn't plugged in stays chosen.
        chosen.forEach(function(name) { if (!names[name]) rows.push(_localAdapterRow(name, name + ' — not found', true)); });
        box.innerHTML = rows.length ? rows.join('') : '<p class="ui-help">No usable network adapters found.</p>';
        _localAdaptersLoaded = true;
    }).catch(function() {
        var box = document.getElementById('local-adapters');
        if (box) box.innerHTML = '<p class="ui-error">' + _netdlgIcon('alert') + '<span>Couldn’t list the network adapters.</span></p>';
    });
}

function showAutoInterfaceConfigSheet() {
    if (_localDialog && _localDialog.isOpen()) _localDialog.close();
    var cur = _localCurrent();
    var v = _localValues(cur);
    // Name and network name always show; the rest stay off a phone, as before.
    var wide = !isCompactLayout();
    _netdlgSelectSet('local-reach', v.scope);
    _netdlgSelectSet('local-address-type', v.addr);
    var dlg = _localDialog = RS.kit.dialog({
        title: 'Local Network',
        size: 'md',
        className: 'netdlg netdlg-local',
        body: '<p>Ratspeak finds people on the same Wi-Fi or wired network. Most people never change these.</p>' +
            _netdlgTextField('local-name', 'Name', { value: v.name, placeholder: 'Local Network', max: 64, help: 'Shown in Connectivity.' }) +
            '<div class="ui-group">' +
                _netdlgDisclosure('local-advanced', 'Advanced', wide ? 'Network name, reach, ports, and adapters.' : 'Network name and reach.', false) +
                '<div class="ui-prop settings-prop-stack" id="local-advanced-body" hidden>' +
                    _netdlgTextField('local-group', 'Network name', { value: v.group, placeholder: 'reticulum', mono: true, max: 63, help: 'Only devices with the same network name find each other.' }) +
                    _netdlgSelectField('local-reach', 'Reach', 'Other choices change the multicast address. They don’t set up multicast routing.') +
                    (wide
                        ? '<div class="ui-field"><span class="ui-label">Ports</span><div class="netdlg-two">' +
                                _netdlgTextField('local-discovery-port', '', { value: v.dport, placeholder: '29716', mode: 'numeric', max: 5, aria: 'Discovery port' }) +
                                _netdlgTextField('local-data-port', '', { value: v.aport, placeholder: '42671', mode: 'numeric', max: 5, aria: 'Data port' }) +
                            '</div><p class="ui-help">Discovery and data ports. Match them on every device.</p></div>' +
                            _netdlgSelectField('local-address-type', 'Address type', 'Use Temporary unless your network needs Permanent.') +
                            '<div class="ui-field"><span class="ui-label" id="local-adapters-label">Network adapters</span>' +
                                '<div class="netdlg-checks" id="local-adapters" role="group" aria-labelledby="local-adapters-label"><p class="ui-status" role="status"><span class="ui-spin ui-spin--ring" aria-hidden="true"></span>Looking for adapters…</p></div>' +
                                '<p class="ui-help">Leave all off to use every adapter with a link-local IPv6 address.</p></div>' +
                            _netdlgTextField('local-bitrate', 'Bitrate', { value: v.bitrate, placeholder: '10', mode: 'decimal', unit: 'Mbps', help: 'Sets the announce cap. 10 Mbps if empty.' })
                        : '') +
                '</div>' +
            '</div>' + _netdlgNoticeSlot(),
        foot: _netdlgFoot(cur ? 'Save' : 'Turn on'),
        onClose: function() { if (_localDialog === dlg) _localDialog = null; }
    });
    var ctx = { cur: cur, saved: v, wide: wide };
    if (wide) _localLoadAdapters(v.devices);
    dlg.dialog.addEventListener('click', function(e) {
        var el = e.target.closest('[data-netdlg]');
        if (!el || el.disabled) return;
        var act = el.getAttribute('data-netdlg');
        if (act === 'disclose') _netdlgSetDisclosure(el.id, el.getAttribute('aria-expanded') !== 'true');
        else if (act === 'select') _netdlgSelectOpen(el);
        else if (act === 'submit') _localSubmit(ctx);
    });
    dlg.dialog.addEventListener('input', function(e) {
        if (e.target.classList.contains('is-invalid')) _netdlgSetError(e.target.id, '');
    });
    dlg.dialog.addEventListener('keydown', function(e) {
        if (e.key !== 'Enter' || e.shiftKey || e.target.tagName !== 'INPUT' || e.target.type === 'checkbox') return;
        e.preventDefault();
        _localSubmit(ctx);
    });
}

function _localFieldError(id, message) {
    _netdlgSetDisclosure('local-advanced', true);
    return _netdlgFieldError(id, message);
}

function _localSubmit(ctx) {
    var dlg = _localDialog;
    if (!dlg || !dlg.isOpen()) return;
    var saved = ctx.saved;
    var wide = ctx.wide;
    _netdlgClearErrors(dlg);
    var form = Object.assign({}, saved, {
        name: _netdlgValue('local-name') || 'Local Network',
        group: _netdlgValue('local-group') || 'reticulum',
        scope: _netdlgSelectGet('local-reach') || 'link'
    });
    if (!/^[A-Za-z0-9_-]{1,63}$/.test(form.group)) {
        _localFieldError('local-group', 'Use letters, numbers, hyphens, and underscores only.');
        return;
    }
    // A phone keeps the wide-layout settings it doesn't show.
    if (wide) {
        var dport = _netdlgPort('local-discovery-port', 29716);
        var aport = _netdlgPort('local-data-port', 42671);
        if (dport === null) return _localFieldError('local-discovery-port', 'Use a port from 1 to 65535.');
        if (aport === null) return _localFieldError('local-data-port', 'Use a port from 1 to 65535.');
        if (dport === aport) return _localFieldError('local-data-port', 'Use a different port from discovery.');
        var bitrate = _netdlgValue('local-bitrate');
        if (bitrate && !(parseFloat(bitrate) > 0)) return _localFieldError('local-bitrate', 'Use a number above 0.');
        form.dport = String(dport);
        form.aport = String(aport);
        form.bitrate = bitrate;
        form.addr = _netdlgSelectGet('local-address-type') || 'temporary';
        if (_localAdaptersLoaded) {
            form.devices = Array.prototype.map.call(dlg.dialog.querySelectorAll('[data-local-adapter]:checked'), function(box) { return box.value; });
        }
    }
    var options = _localOptions(form);

    if (!ctx.cur) {
        dlg.close();
        window._activeProgressDialog = rsProgress({
            title: 'Enabling Local Network',
            message: 'Starting...',
            timeout: 15000,
            timeoutMessage: 'Local Network setup timed out. Check your network settings.'
        });
        RS.invoke('enable_auto_interface', { name: form.name, options: options }).catch(function(err) {
            if (window._activeProgressDialog && window._activeProgressDialog.error) {
                window._activeProgressDialog.error((err && err.message) || 'Failed to enable Local Network');
            }
        });
        return;
    }
    if (form.name === saved.name && JSON.stringify(options) === JSON.stringify(_localOptions(saved))) {
        dlg.close();
        return;
    }
    // There is no in-place update: the interface is replaced, under the runtime's Local Network lock.
    var submitBtn = _netdlgPrimary(dlg);
    _netdlgBusy(submitBtn, 'Saving…');
    RS.invoke('disable_auto_interface', { name: ctx.cur.name }).then(function() {
        // Removed now: a retry only turns it back on.
        ctx.cur = null;
        return RS.invoke('enable_auto_interface', { name: form.name, options: options });
    }).then(function() {
        if (dlg.isOpen()) dlg.close();
        showToast('Local Network updated', 'toast-success', 3000);
    }).catch(function(err) {
        _netdlgBusy(submitBtn, null);
        _netdlgNotice(dlg, (err && err.message) || 'Local Network couldn’t be updated.');
    });
}

function updateAutoToggle() {
    var isEnabled = !!window._autoEnabled;

    ['auto-toggle-indicator', 'settings-auto-toggle', 'conn-auto-toggle'].forEach(function(id) {
        var indicator = document.getElementById(id);
        if (!indicator) return;
        indicator.textContent = isEnabled ? 'ON' : 'OFF';
        indicator.className = 'toggle-indicator' + (isEnabled ? ' on' : '');
    });

    var localCard = document.getElementById('conn-card-local');
    if (localCard) localCard.classList.toggle('active', isEnabled);
}

window._blePeerAvailable = null;

// Android: check BLE perms + request if missing. Other platforms: true.
function _ensureBlePermissions() {
    return new Promise(function(resolve) {
        if (!hasAndroidBridge() || typeof window.RatspeakAndroid.hasBlePermissions !== 'function') {
            resolve(true);
            return;
        }
        try {
            if (window.RatspeakAndroid.hasBlePermissions()) {
                resolve(true);
                return;
            }
        } catch (e) {
        }
        window._onBlePermissionResult = function(granted) {
            window._onBlePermissionResult = null;
            resolve(!!granted);
        };
        try {
            window.RatspeakAndroid.requestBlePermissions();
        } catch (e) {
            window._onBlePermissionResult = null;
            resolve(false);
        }
    });
}

function updateBlePeerToggle() {
    var isEnabled = !!window._blePeerEnabled;

    ['conn-ble-toggle'].forEach(function(id) {
        var indicator = document.getElementById(id);
        if (!indicator) return;
        indicator.textContent = isEnabled ? 'ON' : 'OFF';
        indicator.className = 'toggle-indicator' + (isEnabled ? ' ble-peer-on' : '');
    });

    var bleCard = document.getElementById('conn-card-ble');
    if (bleCard) {
        bleCard.classList.toggle('active', isEnabled);
        bleCard.classList.toggle('needs-install', window._blePeerAvailable === false && !isEnabled);
    }

    if (typeof _refreshBlePeerSectionState === 'function') _refreshBlePeerSectionState();
}

window._bleAvailable = false;
window._bleMissing = [];
window._bleAuthState = null;
window._blePromptPending = false;
RS.invoke('api_ble_available').then(function(data) {
    window._bleAvailable = !!data.available;
    window._bleMissing = data.missing || [];
    window._bleAuthState = data.auth_state || null;
    window._blePromptPending = data.auth_state === 'not_determined';
}).catch(function() {});

// App-Prefs: scheme is undocumented but stable since iOS 8; WKWebView only
// jumps via location.href.
function openIosBluetoothSettings() {
    try { window.location.href = 'App-Prefs:'; } catch (e) {}
}

if (typeof initSheetSwipeDismiss === 'function') {
    initSheetSwipeDismiss('node-modal', 'node-modal-overlay', closeNodeModal);
}

document.addEventListener('keydown', function(e) {
    if (e.key !== 'Escape') return;
    var el = document.getElementById('node-modal');
    if (el && el.classList.contains('open')) closeNodeModal();
});
