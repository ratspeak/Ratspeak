var _modalPreviousFocus = null;

var INTERFACE_SHEET_ICONS = {
    tcp: '<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="M2 12h20"/><path d="M12 2a15 15 0 0 1 4 10 15 15 0 0 1-4 10"/><path d="M12 2a15 15 0 0 0-4 10 15 15 0 0 0 4 10"/></svg>',
    host: '<svg viewBox="0 0 24 24" aria-hidden="true"><rect x="2" y="2" width="20" height="8" rx="2"/><rect x="2" y="14" width="20" height="8" rx="2"/><circle cx="6" cy="6" r="1" fill="currentColor" stroke="none"/><circle cx="6" cy="18" r="1" fill="currentColor" stroke="none"/></svg>',
    backbone: '<svg viewBox="0 0 24 24" aria-hidden="true"><rect x="2" y="2" width="20" height="8" rx="2"/><rect x="2" y="14" width="20" height="8" rx="2"/><circle cx="6" cy="6" r="1" fill="currentColor" stroke="none"/><circle cx="6" cy="18" r="1" fill="currentColor" stroke="none"/></svg>',
    lora: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 20v-14"/><path d="M12 6l-3 3"/><path d="M12 6l3 3"/><path d="M6 14a6 6 0 0 0 0-6"/><path d="M18 14a6 6 0 0 1 0-6"/><path d="M3 16a10 10 0 0 0 0-10"/><path d="M21 16a10 10 0 0 1 0-10"/></svg>',
    local: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 12.55a11 11 0 0 1 14.08 0"/><path d="M1.42 9a16 16 0 0 1 21.16 0"/><path d="M8.53 16.11a6 6 0 0 1 6.95 0"/><circle cx="12" cy="20" r="1" fill="currentColor" stroke="none"/></svg>',
    ble: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M6.5 6.5l11 11L12 23V1l5.5 5.5-11 11"/></svg>',
};

function interfaceSheetIcon(type) {
    return INTERFACE_SHEET_ICONS[type] || '';
}

function interfaceSheetIconTypeForInterface(ifaceType) {
    if (ifaceType === 'rnode') return 'lora';
    if (ifaceType === 'auto' || ifaceType === 'local') return 'local';
    if (ifaceType === 'ble_peer' || ifaceType === 'ble') return 'ble';
    if (ifaceType === 'tcp_server') return 'host';
    if (ifaceType === 'backbone_client' || ifaceType === 'backbone_server') return 'backbone';
    if (ifaceType === 'tcp_client' || ifaceType === 'tcp') return 'tcp';
    return '';
}

function setBottomSheetTitleWithIcon(titleEl, title, iconType) {
    if (!titleEl) return;
    var iconSvg = interfaceSheetIcon(iconType);
    if (!iconSvg) {
        titleEl.classList.remove('bottom-sheet-title-with-icon');
        delete titleEl.dataset.sheetIcon;
        titleEl.textContent = title;
        return;
    }

    titleEl.classList.add('bottom-sheet-title-with-icon');
    titleEl.dataset.sheetIcon = iconType || '';
    titleEl.innerHTML = '';

    var icon = document.createElement('span');
    icon.className = 'bottom-sheet-title-icon';
    icon.innerHTML = iconSvg;
    titleEl.appendChild(icon);

    var label = document.createElement('span');
    label.className = 'bottom-sheet-title-label';
    label.textContent = title;
    titleEl.appendChild(label);
}

var PUBLIC_TCP_SERVERS = [
    { id: 'ratspeak-ruby', name: 'Ruby', host: '1.ratspeak.org', port: 4141, tone: 'ruby', mark_icon: 'gem', tags: ['OFFICIAL'] },
    { id: 'ratspeak-emerald', name: 'Emerald', host: '2.ratspeak.org', port: 4242, tone: 'emerald', mark_icon: 'gem', tags: ['OFFICIAL'], aliases: [{ host: 'rns.ratspeak.org', port: 4242 }] },
    { id: 'ratspeak-diamond', name: 'Diamond', host: '3.ratspeak.org', port: 4343, tone: 'diamond', mark_icon: 'gem', tags: ['OFFICIAL'] },
    { id: 'beleth', name: 'Beleth', host: 'rns.beleth.net', port: 4242, tone: 'beleth', mark: 'B', tags: ['UNOFFICIAL'] },
    { id: 'rmap', name: 'RMAP', host: 'rmap.world', port: 4242, tone: 'rmap', mark: 'R', tags: ['UNOFFICIAL'] },
];

function _tcpServerKey(host, port) {
    return String(host || '').trim().toLowerCase() + ':' + (parseInt(port, 10) || 4242);
}

function _publicServerEndpointKeys(server) {
    var endpoints = [{ host: server.host, port: server.port }].concat(server.aliases || []);
    return endpoints.map(function(endpoint) {
        return _tcpServerKey(endpoint.host, endpoint.port);
    });
}

var PUBLIC_TCP_SERVER_KEYS = PUBLIC_TCP_SERVERS.reduce(function(acc, server) {
    _publicServerEndpointKeys(server).forEach(function(key) {
        acc[key] = true;
    });
    return acc;
}, {});
var _connectPendingPublicServerKey = null;

function _isPublicTcpServer(host, port) {
    return !!PUBLIC_TCP_SERVER_KEYS[_tcpServerKey(host, port)];
}

function _publicServerMatchesEndpoint(server, host, port) {
    var endpointKey = _tcpServerKey(host, port);
    return _publicServerEndpointKeys(server).indexOf(endpointKey) >= 0;
}

var PUBLIC_SERVER_ARROW_ICON = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 12h14"/><path d="m13 6 6 6-6 6"/></svg>';
var PUBLIC_SERVER_CHECK_ICON = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="m5 12 4 4L19 6"/></svg>';
var PUBLIC_SERVER_GEM_ICON = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M7.2 3h9.6L22 8.8 12 21 2 8.8 7.2 3Z"/><path d="M7.2 3 9.5 8.8 12 21"/><path d="M16.8 3 14.5 8.8 12 21"/><path d="M2 8.8h20"/><path d="M9.5 8.8 12 3l2.5 5.8"/></svg>';

function _publicServerMarkHtml(server) {
    if (server.mark_icon === 'gem') {
        return '<span class="public-server-mark public-server-mark--gem" aria-hidden="true">' + PUBLIC_SERVER_GEM_ICON + '</span>';
    }
    return '<span class="public-server-mark" aria-hidden="true">' + escapeHtml(server.mark || server.name.charAt(0)) + '</span>';
}

function _setConnectSubmitBase(btn, text) {
    if (!btn) return;
    btn.textContent = text || 'Connect';
    btn.className = 'nr-btn w-full mt-4';
    btn.disabled = false;
}

function _trapFocus(modalEl) {
    _modalPreviousFocus = document.activeElement;
    var focusable = modalEl.querySelectorAll(
        'button, [href], input:not([type="hidden"]), select, textarea, [tabindex]:not([tabindex="-1"])'
    );
    if (focusable.length > 0) {
        var first = focusable[0];
        if (!isMobile() || (first.tagName !== 'INPUT' && first.tagName !== 'TEXTAREA' && first.tagName !== 'SELECT')) {
            first.focus();
        }
    }

    modalEl._focusTrapHandler = function(e) {
        if (e.key !== 'Tab') return;
        var items = modalEl.querySelectorAll(
            'button, [href], input:not([type="hidden"]), select, textarea, [tabindex]:not([tabindex="-1"])'
        );
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
    if (_modalPreviousFocus && _modalPreviousFocus.focus) {
        _modalPreviousFocus.focus();
        _modalPreviousFocus = null;
    }
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
        showToast(message, 'toast-red', 8000);
    });
}

var _rnodeConnectionType = 'serial';
var RNODE_TCP_DEFAULT_PORT = 7633;
var _bleSelectedDevice = null;
var _androidUsbSelectedDevice = null;
var _rnodeEditContext = null;
var _connectEditContext = null;
var _hostEditContext = null;
var _backboneHostEditContext = null;
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
    var select = document.getElementById('rnode-region');
    return select ? select.value : (_rnodeCatalogDefaults.region || 'americas');
}

function _rnodeSelectedPresetKey() {
    var select = document.getElementById('rnode-preset');
    return select ? select.value : (_rnodeCatalogDefaults.preset || 'medium_fast');
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
        return { error: 'Frequency must be between ' + _rnodeFormatMhz(limits.frequencyMin) + ' and ' + _rnodeFormatMhz(limits.frequencyMax) + ' MHz' };
    }
    if (!bw || bw < limits.bandwidthMin || bw > limits.bandwidthMax) {
        return { error: 'Bandwidth must be between ' + _rnodeFormatKhz(limits.bandwidthMin) + ' and ' + _rnodeFormatKhz(limits.bandwidthMax) + ' kHz' };
    }
    if (isNaN(sf) || sf < limits.sfMin || sf > limits.sfMax) {
        return { error: 'Spreading factor must be between ' + limits.sfMin + ' and ' + limits.sfMax };
    }
    if (isNaN(cr) || cr < limits.crMin || cr > limits.crMax) {
        return { error: 'Coding rate must be between ' + limits.crMin + ' and ' + limits.crMax };
    }
    if (isNaN(tx) || tx < limits.txMin || tx > limits.txMax) {
        return { error: 'TX power must be between ' + limits.txMin + ' and ' + limits.txMax + ' dBm' };
    }
    var airtimeShort = _rnodeParseAirtimePercent('rnode-airtime-short');
    var airtimeLong = _rnodeParseAirtimePercent('rnode-airtime-long');
    if (airtimeShort !== null && (isNaN(airtimeShort) || airtimeShort < 0 || airtimeShort > 100)) {
        return { error: 'Short-term airtime limit must be between 0 and 100 %' };
    }
    if (airtimeLong !== null && (isNaN(airtimeLong) || airtimeLong < 0 || airtimeLong > 100)) {
        return { error: 'Long-term airtime limit must be between 0 and 100 %' };
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
    if (message) {
        error.textContent = message;
        error.style.display = '';
    } else {
        error.textContent = '';
        error.style.display = 'none';
    }
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
    if (els.controls) els.controls.style.display = enabled ? '' : 'none';
    if (!enabled) {
        _rnodeSetPublicMapError('');
    } else {
        _rnodeSetPublicMapStatus();
    }
}

function _rnodeResetPublicMap() {
    var els = _rnodePublicMapElements();
    if (els.section) els.section.style.display = 'none';
    if (els.latitude) els.latitude.value = '';
    if (els.longitude) els.longitude.value = '';
    _rnodeSetPublicMapError('');
    _rnodeSetPublicMapEnabled(false);
}

function _rnodeLoadPublicMap(editIface) {
    var els = _rnodePublicMapElements();
    if (!els.section) return;
    els.section.style.display = editIface ? '' : 'none';
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
    if (!els.section || els.section.style.display === 'none' || !els.checkbox || !els.checkbox.checked) {
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

function _rnodeUpdateRadioHints() {
    var freq = _rnodeParseFrequencyHz(document.getElementById('rnode-frequency').value);
    var regionKey = _rnodeSelectedRegionKey();
    var presetKey = _rnodeSelectedPresetKey();
    var region = _rnodeRegions && _rnodeRegions[regionKey];
    var preset = _rnodePresets && _rnodePresets[presetKey];
    var freqHint = document.getElementById('rnode-frequency-hint');
    var warn = document.getElementById('rnode-frequency-warning');
    var presetHint = document.getElementById('rnode-preset-hint');

    if (freqHint) {
        freqHint.textContent = region
            ? 'Default ' + _rnodeFormatMhz(region.freq) + ' MHz. Config is written as ' + (freq || region.freq) + ' Hz.'
            : 'Custom center frequency. Config is written in Hz.';
    }
    if (warn) {
        if (freq && region && !_rnodeRegionContainsFrequency(regionKey, freq)) {
            warn.textContent = 'This frequency is outside the selected band range of ' +
                _rnodeFormatMhz(region.min) + '-' + _rnodeFormatMhz(region.max) +
                ' MHz. Check hardware support and local regulations before transmitting.';
            warn.style.display = '';
        } else {
            warn.style.display = 'none';
            warn.textContent = '';
        }
    }
    if (presetHint) {
        presetHint.textContent = preset
            ? 'SF' + preset.sf + ', BW ' + _rnodeFormatKhz(preset.bw) + ' kHz, CR ' + preset.cr + ', TX ' + preset.tx + ' dBm.'
            : 'Custom radio parameters.';
    }
}

function _rnodeRefreshRegionFromFrequency() {
    var freq = _rnodeParseFrequencyHz(document.getElementById('rnode-frequency').value);
    var select = document.getElementById('rnode-region');
    if (!freq || !select) {
        _rnodeUpdateRadioHints();
        return;
    }
    var current = select.value;
    if (current !== _RNODE_CUSTOM_REGION_KEY && _rnodeRegionContainsFrequency(current, freq)) {
        _rnodeUpdateRadioHints();
        return;
    }
    select.value = _rnodeRegionForFrequency(freq);
    _rnodeUpdateRadioHints();
}

function _rnodeRefreshPresetFromAdvanced() {
    var bw = _rnodeParseBandwidthHz(document.getElementById('rnode-bandwidth').value);
    var sf = parseInt(document.getElementById('rnode-spreading-factor').value, 10);
    var cr = parseInt(document.getElementById('rnode-coding-rate').value, 10);
    var tx = parseInt(document.getElementById('rnode-tx-power').value, 10);
    var select = document.getElementById('rnode-preset');
    if (!bw || isNaN(sf) || isNaN(cr) || isNaN(tx) || !select) {
        _rnodeUpdateRadioHints();
        return;
    }
    select.value = _rnodePresetForParams(bw, sf, cr, tx);
    _rnodeUpdateRadioHints();
}

function _rnodeApplyRadioControls(regionKey, presetKey, freq, bw, sf, cr, tx, openAdvanced) {
    var regionSelect = document.getElementById('rnode-region');
    var presetSelect = document.getElementById('rnode-preset');
    var advanced = document.getElementById('rnode-advanced');
    if (regionSelect) regionSelect.value = regionKey || _rnodeCatalogDefaults.region || 'americas';
    if (presetSelect) presetSelect.value = presetKey || _rnodeCatalogDefaults.preset || 'medium_fast';
    _rnodeSetFrequency(freq);
    _rnodeSetAdvancedParams(bw, sf, cr, tx);
    if (advanced) advanced.open = !!openAdvanced;
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

function _rnodeNormaliseInterfaceMode(mode) {
    mode = String(mode || 'full').trim().toLowerCase();
    if (mode === 'gw') mode = 'gateway';
    if (mode === 'ap' || mode === 'accesspoint' || mode === 'access point') mode = 'access_point';
    return _RNODE_INTERFACE_MODE_VALUES[mode] ? mode : 'full';
}

function _rnodeSetInterfaceMode(mode) {
    var select = document.getElementById('rnode-interface-mode');
    if (!select) return;
    select.value = _rnodeNormaliseInterfaceMode(mode);
}

function _rnodeReadInterfaceMode() {
    var select = document.getElementById('rnode-interface-mode');
    return _rnodeNormaliseInterfaceMode(select ? select.value : 'full');
}

function _rnodeDeveloperModeEnabled() {
    return typeof window.ratspeakDeveloperModeEnabled === 'function' &&
        window.ratspeakDeveloperModeEnabled();
}

function _developerModeEnabled() {
    return _rnodeDeveloperModeEnabled();
}

function _rnodeSyncInterfaceModeVisibility() {
    var field = document.getElementById('rnode-mode-field');
    if (field) field.style.display = _rnodeDeveloperModeEnabled() ? '' : 'none';
}

window.addEventListener('ratspeak-developer-mode-changed', _rnodeSyncInterfaceModeVisibility);
window.addEventListener('ratspeak-developer-mode-changed', _syncConnectAdvancedVisibility);

function openRnodeModal(mode, editIface) {
    mode = mode || 'ble';
    _rnodeEditContext = null;
    // iOS MFi blocks USB serial; Android uses USB-OTG JNI; desktop uses serialport.
    var serialToggle = document.getElementById('rnode-toggle-serial');
    var androidUsbToggle = document.getElementById('rnode-toggle-android-usb');
    var tcpToggle = document.getElementById('rnode-toggle-tcp');
    if (serialToggle) {
        var hideSerial = isIOS() || (isAndroid() && hasAndroidBridge());
        serialToggle.style.display = hideSerial ? 'none' : '';
    }
    if (androidUsbToggle) {
        androidUsbToggle.style.display = (isAndroid() && hasAndroidBridge()) ? '' : 'none';
    }
    if (tcpToggle) tcpToggle.style.display = '';
    if (isIOS() && mode !== 'tcp') mode = 'ble';
    if (isAndroid() && hasAndroidBridge() && mode === 'serial') mode = 'android-usb';
    _rnodeConnectionType = mode;
    _bleSelectedDevice = null;
    _androidUsbSelectedDevice = null;

    var step1 = document.getElementById('rnode-step-1');
    var step2 = document.getElementById('rnode-step-2');
    if (step1) step1.style.display = '';
    if (step2) step2.style.display = 'none';

    var titleEl = document.querySelector('#rnode-modal .bottom-sheet-title');
    setBottomSheetTitleWithIcon(titleEl, editIface ? 'Edit LoRa Device' : 'Add LoRa Device', 'lora');
    document.getElementById('rnode-iface-name').value = '';
    var tcpInput = document.getElementById('rnode-tcp-endpoint');
    if (tcpInput) tcpInput.value = '';
    var catalogReady = loadRnodePresetCatalog();
    _rnodeApplyDefaultRadioControls();
    _rnodeSetAirtimeLimits(null, null);
    _rnodeSetInterfaceMode('full');
    _rnodeSyncInterfaceModeVisibility();
    _rnodeResetPublicMap();
    _bleSelectedDevice = null;
    _selectedSerialPort = null;

    var bleList = document.getElementById('ble-device-list');
    if (bleList) bleList.innerHTML = '<div class="ble-scan-placeholder">Click "Scan" to find nearby RNode devices.</div>';

    var bleHint = document.getElementById('rnode-ble-hint');
    if (bleHint) {
        if (window._bleAvailable) {
            bleHint.textContent = '';
        } else if (window._bleAuthState === 'denied' || window._bleAuthState === 'restricted') {
            bleHint.innerHTML = 'denied — <a href="#" id="ble-open-settings" style="text-decoration:underline;">Open Settings</a>';
            var settingsLink = document.getElementById('ble-open-settings');
            if (settingsLink) settingsLink.addEventListener('click', function(e) {
                e.preventDefault();
                openIosBluetoothSettings();
            });
        } else {
            bleHint.textContent = 'unavailable';
        }
    }
    var bleToggleBtn = document.getElementById('rnode-toggle-ble');
    if (bleToggleBtn) bleToggleBtn.classList.toggle('needs-install', !window._bleAvailable);

    setRnodeConnectionType(mode);

    if (editIface) {
        var port = _ifaceString(editIface, 'port', '');
        _rnodeEditContext = { oldName: editIface.name || '', port: port };
        _rnodeSetInterfaceMode(editIface.mode || editIface.interface_mode || 'full');
        if (port.indexOf('ble://') === 0) {
            var addr = port.substring(6);
            _bleSelectedDevice = { name: editIface.name || 'LoRa Radio', address: addr };
            if (bleList) {
                bleList.innerHTML = '<button type="button" class="ble-device-btn selected">' +
                    '<span class="ble-device-name">' + escapeHtml(editIface.name || 'LoRa Radio') + '</span>' +
                    '<span class="ble-device-meta"><span class="ble-device-addr">' + escapeHtml(addr) + '</span></span>' +
                    '</button>';
            }
        } else if (port.indexOf('androidusb://') === 0) {
            var dev = port.substring('androidusb://'.length);
            _androidUsbSelectedDevice = { device_name: dev, product: editIface.name || 'LoRa Radio' };
        } else if (_rnodeIsTcpPort(port)) {
            if (tcpInput) tcpInput.value = _rnodeTcpInputValue(port);
        } else {
            _selectedSerialPort = { device: port, description: port };
            var serialSel = document.getElementById('rnode-port');
            if (serialSel) {
                serialSel.innerHTML = '<option value="' + escapeHtml(port) + '">' + escapeHtml(port) + '</option>';
                serialSel.value = port;
            }
        }

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
        if (step1) step1.style.display = 'none';
        if (step2) step2.style.display = '';
        var submit = document.getElementById('rnode-submit-btn');
        if (submit) submit.textContent = 'Save Changes';
        rnodeUpdateNextBtn();
    } else {
        _rnodeResetPublicMap();
        var submitBtn = document.getElementById('rnode-submit-btn');
        if (submitBtn) submitBtn.textContent = 'Add Radio';
        catalogReady.then(_rnodeApplyDefaultRadioControls).catch(function() {});
    }

    RS.ui.openExistingSheet('rnode-modal', 'rnode-modal-overlay');
}

function closeRnodeModal() {
    RS.ui.closeExistingSheet('rnode-modal', 'rnode-modal-overlay');
    _bleSelectedDevice = null;
    _selectedSerialPort = null;
    _androidUsbSelectedDevice = null;
    _rnodeEditContext = null;
    _rnodeResetPublicMap();
    var tcpInput = document.getElementById('rnode-tcp-endpoint');
    if (tcpInput) tcpInput.value = '';
    var titleEl = document.querySelector('#rnode-modal .bottom-sheet-title');
    setBottomSheetTitleWithIcon(titleEl, 'Add LoRa Device', 'lora');
}

function setRnodeConnectionType(type) {
    _rnodeConnectionType = type;

    var serialBtn = document.getElementById('rnode-toggle-serial');
    var bleBtn = document.getElementById('rnode-toggle-ble');
    var usbBtn = document.getElementById('rnode-toggle-android-usb');
    var tcpBtn = document.getElementById('rnode-toggle-tcp');
    if (serialBtn) serialBtn.classList.toggle('active', type === 'serial');
    if (bleBtn) bleBtn.classList.toggle('active', type === 'ble');
    if (usbBtn) usbBtn.classList.toggle('active', type === 'android-usb');
    if (tcpBtn) tcpBtn.classList.toggle('active', type === 'tcp');

    var serialSection = document.getElementById('rnode-serial-section');
    var bleSection = document.getElementById('rnode-ble-section');
    var usbSection = document.getElementById('rnode-android-usb-section');
    var tcpSection = document.getElementById('rnode-tcp-section');
    if (serialSection) serialSection.style.display = type === 'serial' ? '' : 'none';
    if (bleSection) bleSection.style.display = type === 'ble' ? '' : 'none';
    if (usbSection) usbSection.style.display = type === 'android-usb' ? '' : 'none';
    if (tcpSection) tcpSection.style.display = type === 'tcp' ? '' : 'none';

    _bleSelectedDevice = null;
    _selectedSerialPort = null;
    _androidUsbSelectedDevice = null;

    if (type === 'serial') {
        document.getElementById('rnode-port').value = '';
        refreshRnodeSerialPorts();
    } else if (type === 'android-usb') {
        refreshAndroidUsbDevices();
    }

    updateRnodeHandoffHints();
    rnodeUpdateNextBtn();
}

// Mirrors `commands/interfaces.rs::teardown_rnode_handoff_broadcast`.
function updateRnodeHandoffHints() {
    var data = window._hubInterfacesData;
    var rnodes = (data && Array.isArray(data.rnode)) ? data.rnode : [];
    var hasBle = false, hasUsb = false;
    for (var i = 0; i < rnodes.length; i++) {
        var p = rnodes[i].port || '';
        if (p.indexOf('ble://') === 0) hasBle = true;
        else if (p.indexOf('androidusb://') === 0) hasUsb = true;
    }
    var bleHint = document.getElementById('rnode-handoff-hint-ble');
    var usbHint = document.getElementById('rnode-handoff-hint-usb');
    if (bleHint) bleHint.style.display = (_rnodeConnectionType === 'ble' && hasUsb) ? '' : 'none';
    if (usbHint) usbHint.style.display = (_rnodeConnectionType === 'android-usb' && hasBle) ? '' : 'none';
}

var _selectedSerialPort = null;

function rnodeUpdateNextBtn() {
    var btn = document.getElementById('rnode-next-btn');
    if (!btn) return;
    var hasDevice = false;
    if (_rnodeConnectionType === 'ble') {
        hasDevice = !!_bleSelectedDevice;
    } else if (_rnodeConnectionType === 'android-usb') {
        hasDevice = !!_androidUsbSelectedDevice;
    } else if (_rnodeConnectionType === 'tcp') {
        var tcpInput = document.getElementById('rnode-tcp-endpoint');
        hasDevice = !!(tcpInput && _normaliseRnodeTcpEndpoint(tcpInput.value).port);
    } else {
        hasDevice = !!_selectedSerialPort;
    }
    btn.disabled = !hasDevice;
}

function rnodeWizardNext() {
    var step1 = document.getElementById('rnode-step-1');
    var step2 = document.getElementById('rnode-step-2');
    if (!step1 || !step2) return;

    var nameInput = document.getElementById('rnode-iface-name');

    if (_rnodeConnectionType === 'ble' && _bleSelectedDevice) {
        if (!nameInput.value.trim()) nameInput.value = _bleSelectedDevice.name || 'LoRa Radio';
    } else if (_rnodeConnectionType === 'android-usb' && _androidUsbSelectedDevice) {
        if (!nameInput.value.trim()) nameInput.value = 'LoRa Radio';
    } else if (_rnodeConnectionType === 'tcp') {
        var tcpInput = document.getElementById('rnode-tcp-endpoint');
        var tcpEndpoint = _normaliseRnodeTcpEndpoint(tcpInput ? tcpInput.value : '');
        if (tcpEndpoint.error) {
            showPreConditionToast(tcpEndpoint.error);
            rnodeUpdateNextBtn();
            return;
        }
        if (tcpInput) tcpInput.value = tcpEndpoint.label;
        if (!nameInput.value.trim()) nameInput.value = 'LoRa Radio';
    } else if (_selectedSerialPort) {
        if (!nameInput.value.trim()) nameInput.value = 'LoRa Radio';
    }

    // Bonding happens implicitly during add_lora_interface; OS pair dialog
    // pops mid-connect (Linux uses ble_rnode_passkey_prompt instead).
    step1.style.display = 'none';
    step2.style.display = '';
}

function rnodeWizardBack() {
    var step1 = document.getElementById('rnode-step-1');
    var step2 = document.getElementById('rnode-step-2');
    if (step1) step1.style.display = '';
    if (step2) step2.style.display = 'none';
}

function refreshRnodeSerialPorts() {
    var sel = document.getElementById('rnode-port');
    if (!sel) return;
    sel.innerHTML = '<option value="">Scanning...</option>';
    RS.invoke('api_serial_ports').then(function(ports) {
        if (ports.length === 0) {
            sel.innerHTML = '<option value="">No devices found</option>';
        } else {
            sel.innerHTML = '<option value="">Select device...</option>' +
                ports.map(function(p) {
                    var base = p.description !== p.device ? p.description + ' (' + p.device + ')' : p.device;
                    var label = p.permission_denied ? base + ' — permission denied' : base;
                    return '<option value="' + escapeHtml(p.device) + '">' + escapeHtml(label) + '</option>';
                }).join('');
            if (ports.some(function(p) { return p.permission_denied; })) {
                showToast(
                    'A USB-Serial radio was detected but is not user-readable. ' +
                    'Install the udev rules: sudo cp 99-ratspeak.rules /etc/udev/rules.d/ && ' +
                    'sudo udevadm control --reload && sudo udevadm trigger',
                    'toast-yellow',
                    8000
                );
            }
        }
    }).catch(function() {
        showToast('Failed to scan serial ports', 'toast-red', 3000);
        sel.innerHTML = '<option value="">Error scanning ports</option>';
    });
}

// Enumeration via Kotlin bridge so list populates before USB permission;
// Rust claims the device only after user picks it.
function refreshAndroidUsbDevices() {
    var list = document.getElementById('android-usb-device-list');
    if (!list) return;
    if (!hasAndroidBridge() || typeof window.RatspeakAndroid.listUsbDevices !== 'function') {
        list.innerHTML = '<div class="ble-scan-placeholder">USB not available on this platform.</div>';
        return;
    }
    list.innerHTML = '<div class="ble-scan-placeholder"><span class="loading-spinner"></span> Searching for USB devices...</div>';
    var devices = [];
    try {
        var raw = window.RatspeakAndroid.listUsbDevices();
        devices = JSON.parse(raw || '[]');
    } catch (e) {
        list.innerHTML = '<div class="ble-scan-placeholder inline-error">' +
            'Failed to enumerate USB devices: ' + escapeHtml(String(e && e.message || e)) + '</div>';
        return;
    }
    if (!devices.length) {
        list.innerHTML = '<div class="ble-scan-placeholder">No USB devices detected. Check the OTG cable and that the RNode is powered.</div>';
        return;
    }
    list.innerHTML = devices.map(function(d, idx) {
        var title = d.product || d.manufacturer || d.device_name;
        var hex = 'VID ' + d.vid.toString(16).toUpperCase().padStart(4, '0') +
                  '  PID ' + d.pid.toString(16).toUpperCase().padStart(4, '0');
        return '<button type="button" class="ble-device-btn" data-usb-idx="' + idx + '">' +
               '<span class="ble-device-name">' + escapeHtml(title) + '</span>' +
               '<span class="ble-device-meta">' +
                   '<span class="ble-device-addr">' + escapeHtml(hex) + '</span>' +
                   '<span class="ble-badge bonded">USB</span>' +
               '</span>' +
               '</button>';
    }).join('');
    Array.prototype.forEach.call(list.querySelectorAll('.ble-device-btn'), function(row) {
        row.addEventListener('click', function() {
            var idx = parseInt(row.getAttribute('data-usb-idx'), 10);
            _androidUsbSelectedDevice = devices[idx] || null;
            Array.prototype.forEach.call(list.querySelectorAll('.ble-device-btn'), function(r) {
                r.classList.remove('selected');
            });
            row.classList.add('selected');
            rnodeUpdateNextBtn();
        });
    });
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

function _populateRnodeSelect(selectId, entries, defaultKey) {
    var select = document.getElementById(selectId);
    if (!select) return;
    var previous = select.value || defaultKey || '';
    var choices = entries.slice();
    if (selectId === 'rnode-region') {
        choices.push({ key: _RNODE_CUSTOM_REGION_KEY, label: 'Custom frequency' });
    } else if (selectId === 'rnode-preset') {
        choices.push({ key: _RNODE_CUSTOM_PRESET_KEY, label: 'Custom radio parameters' });
    }
    select.innerHTML = choices.map(function(entry) {
        return '<option value="' + escapeHtml(entry.key) + '">' +
            escapeHtml(entry.label || entry.key) + '</option>';
    }).join('');
    select.value = choices.some(function(entry) { return entry.key === previous; })
        ? previous
        : (defaultKey || (choices[0] && choices[0].key) || '');
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

function scanBleDevices() {
    var list = document.getElementById('ble-device-list');
    var scanBtn = document.getElementById('ble-scan-btn');

    if (hasAndroidBridge()) {
        scanBtn.textContent = 'Preparing...';
        scanBtn.disabled = true;
        window._onBlePermissionResult = function(granted) {
            window._onBlePermissionResult = null;
            if (granted) {
                _doBleScan(list, scanBtn);
            } else {
                scanBtn.textContent = 'Start Scan';
                scanBtn.disabled = false;
                list.innerHTML = '<div class="ble-scan-placeholder inline-error">' +
                    'Bluetooth permission denied. Grant permission in system settings to scan for LoRa radios.</div>';
            }
        };
        try {
            window.RatspeakAndroid.requestBlePermissions();
        } catch (e) {
            window._onBlePermissionResult = null;
            scanBtn.textContent = 'Start Scan';
            scanBtn.disabled = false;
        }
        return;
    }

    _doBleScan(list, scanBtn);
}

function _doBleScan(list, scanBtn) {
    scanBtn.textContent = 'Scanning...';
    scanBtn.disabled = true;
    list.innerHTML = '<div class="ble-scan-placeholder"><span class="loading-spinner"></span> Scanning...</div>';

    var resetScanBtn = function() {
        scanBtn.textContent = 'Scan Again';
        scanBtn.disabled = false;
    };
    // Watchdog for dropped callbacks / bridge crashes.
    var watchdog = setTimeout(function() {
        resetScanBtn();
        if (list.querySelector('.loading-spinner')) {
            list.innerHTML = '<div class="ble-scan-placeholder">Scan timed out. Try again.</div>';
        }
    }, 8000);
    var clearWatchdog = function() { clearTimeout(watchdog); };

    // btleplug fails on Android 14+; route through native Kotlin scanner.
    if (hasAndroidBridge() && typeof window.RatspeakAndroid.scanBleDevices === 'function') {
        window._onNativeBleScanResult = function(data) {
            window._onNativeBleScanResult = null;
            clearWatchdog();
            resetScanBtn();

            if (data.error) {
                list.innerHTML = '<div class="ble-scan-placeholder inline-error">' +
                    escapeHtml(data.error) + '</div>';
                return;
            }

            var rnodeDevices = (data.devices || []).filter(function(d) {
                return d.device_type === 'rnode';
            });
            if (typeof renderBleDeviceList === 'function') {
                renderBleDeviceList(rnodeDevices);
            }
        };
        try {
            window.RatspeakAndroid.scanBleDevices(5000);
        } catch (e) {
            window._onNativeBleScanResult = null;
            clearWatchdog();
            resetScanBtn();
            list.innerHTML = '<div class="ble-scan-placeholder inline-error">Failed to start BLE scan.</div>';
        }
        return;
    }

    // Desktop/iOS: invoke returns scan results synchronously.
    RS.invoke('scan_ble_devices').then(function(result) {
        clearWatchdog();
        resetScanBtn();
        if (result && Array.isArray(result.devices)) {
            renderBleDeviceList(result.devices);
        } else if (result && result.error) {
            renderBleDeviceList([]);
        }
    }).catch(function() {
        clearWatchdog();
        resetScanBtn();
        renderBleDeviceList([]);
    });
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
    var list = document.getElementById('ble-device-list');

    if (devices.length === 0) {
        list.innerHTML = '<div class="ble-scan-placeholder">No RNode devices found. Make sure your device is powered on and paired.</div>';
        return;
    }

    var showBondBadge = _bondBadgeReliable();

    // Bonded-first only when the bond flag is meaningful; else RSSI-only.
    devices.sort(function(a, b) {
        if (showBondBadge && a.bonded !== b.bonded) return b.bonded ? 1 : -1;
        return (b.rssi || -100) - (a.rssi || -100);
    });

    list.innerHTML = '';
    devices.forEach(function(dev) {
        var btn = document.createElement('button');
        btn.className = 'ble-device-btn' + (showBondBadge && !dev.bonded ? ' unbonded' : '');
        btn.type = 'button';

        var rssiText = '';
        if (dev.rssi !== null && dev.rssi !== undefined) {
            var strength = Math.min(Math.max((dev.rssi + 100) / 60 * 100, 5), 100);
            var rssiColor = strength > 60 ? 'var(--status-online)' : (strength > 30 ? 'var(--status-warning)' : 'var(--status-error)');
            rssiText = '<span class="ble-rssi" style="color:' + rssiColor + ';">' + dev.rssi + ' dBm</span>';
        }

        var bondBadge = '';
        if (showBondBadge) {
            bondBadge = dev.bonded
                ? '<span class="ble-badge bonded">Paired</span>'
                : '<span class="ble-badge unbonded">Not Paired</span>';
        }

        btn.innerHTML = '<span class="ble-device-name">' + escapeHtml(dev.name) + '</span>' +
            '<span class="ble-device-meta">' +
                '<span class="ble-device-addr">' + escapeHtml(dev.address) + '</span>' +
                rssiText + bondBadge +
            '</span>';

        btn.addEventListener('click', function() {
            _bleSelectedDevice = dev;
            list.querySelectorAll('.ble-device-btn').forEach(function(b) {
                b.classList.remove('selected');
            });
            btn.classList.add('selected');
            rnodeUpdateNextBtn();
        });

        list.appendChild(btn);
    });
}

function submitRnodeInterface() {
    var name = document.getElementById('rnode-iface-name').value.trim();
    if (!name) {
        showPreConditionToast('Please enter a name');
        return;
    }

    var port = '';
    if (_rnodeConnectionType === 'ble' && _bleSelectedDevice) {
        port = 'ble://' + _bleSelectedDevice.address;
    } else if (_rnodeConnectionType === 'android-usb' && _androidUsbSelectedDevice) {
        port = 'androidusb://' + _androidUsbSelectedDevice.device_name;
    } else if (_rnodeConnectionType === 'tcp') {
        var tcpEndpoint = _normaliseRnodeTcpEndpoint(document.getElementById('rnode-tcp-endpoint').value);
        if (tcpEndpoint.error) {
            showPreConditionToast(tcpEndpoint.error);
            return;
        }
        port = tcpEndpoint.port;
    } else if (_selectedSerialPort) {
        port = _selectedSerialPort.device;
    }
    if (!port) {
        showPreConditionToast('No device selected. Go back and select a device');
        return;
    }

    var radioSettings = _rnodeReadRadioSettings();
    if (radioSettings.error) {
        showPreConditionToast(radioSettings.error);
        return;
    }

    var publicMapSettings = null;
    if (_rnodeEditContext) {
        publicMapSettings = _rnodeReadPublicMapSettings();
        if (publicMapSettings.error) {
            _rnodeSetPublicMapError(publicMapSettings.error);
            showPreConditionToast(publicMapSettings.error);
            return;
        }
        _rnodeSetPublicMapError('');
    }

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
                        var err = result && result.error ? result.error : 'USB permission denied';
                        showToast(err, 'toast-red', 4000);
                        resolve(false);
                    }
                };
                try { window.RatspeakAndroid.requestUsbPermission(devName); }
                catch (e) { window._onUsbPermissionResult = null; resolve(false); }
            });
        }
    }

    proceed.then(function(permOk) {
        if (!permOk) return;
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
                    // Drop half-written config so the list has no orphan.
                    if (!isEdit) RS.invoke('cancel_ble_connect', { name: bleName }).catch(function() {});
                    window._activeProgressDialog = null;
                },
            });
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
                showToast((err && err.message) || 'Failed to configure LoRa interface', 'toast-red', 8000);
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

function setConnectTab(tab) {
    tab = tab === 'custom' ? 'custom' : 'public';
    var buttons = document.querySelectorAll('#connect-tab-toggle [data-connect-tab]');
    buttons.forEach(function(btn) {
        var active = btn.dataset.connectTab === tab;
        btn.classList.toggle('active', active);
        btn.setAttribute('aria-selected', active ? 'true' : 'false');
    });
    var publicPanel = document.getElementById('connect-public-panel');
    var customPanel = document.getElementById('connect-custom-panel');
    if (publicPanel) publicPanel.classList.toggle('active', tab === 'public');
    if (customPanel) customPanel.classList.toggle('active', tab === 'custom');
    if (tab === 'custom' && !_connectEditContext) {
        var nameEl = document.getElementById('connect-name');
        if (nameEl) nameEl.value = '';
    }
    var body = document.querySelector('#connect-modal .bottom-sheet-body');
    if (body) body.scrollTop = 0;
}

function _connectMatchingPublicInterface(server, ifaces) {
    ifaces = ifaces || window._hubInterfacesData || window._cachedConfigIfaces || {};
    var clients = Array.isArray(ifaces.tcp_client) ? ifaces.tcp_client : [];
    for (var i = 0; i < clients.length; i++) {
        var iface = clients[i] || {};
        if (!_publicServerMatchesEndpoint(server, iface.target_host, iface.target_port)) continue;
        var live = (typeof getInterfaceLiveStatus === 'function') ? getInterfaceLiveStatus(iface.name || '') : null;
        return {
            iface: iface,
            online: live ? live.online !== false : false,
        };
    }
    return null;
}

function renderPublicTcpServers(ifaces) {
    var container = document.getElementById('public-server-list');
    if (!container) return;
    container.innerHTML = '';

    PUBLIC_TCP_SERVERS.forEach(function(server) {
        var match = _connectMatchingPublicInterface(server, ifaces);
        var connected = !!(match && match.online);
        var pending = _connectPendingPublicServerKey === _tcpServerKey(server.host, server.port) && !connected;
        var configured = !!match && !connected;
        var action = pending ? 'Connecting...' : (connected ? 'Connected' : 'Connect');
        var actionIcon = connected ? PUBLIC_SERVER_CHECK_ICON : PUBLIC_SERVER_ARROW_ICON;

        var btn = document.createElement('button');
        btn.type = 'button';
        btn.className = 'public-server-card public-server-card--' + server.tone +
            (pending ? ' is-pending' : '') +
            (connected ? ' is-connected' : '') +
            (configured ? ' is-added' : '');
        btn.disabled = connected || pending;
        btn.setAttribute('aria-label', action + ' ' + server.name);
        btn.title = action + ' ' + server.name;

        var tags = (server.tags || []).map(function(tag) {
            return '<span class="public-server-tag">' + escapeHtml(tag) + '</span>';
        }).join('');

        btn.innerHTML =
            _publicServerMarkHtml(server) +
            '<span class="public-server-main">' +
                '<span class="public-server-name">' + escapeHtml(server.name) + '</span>' +
                '<span class="public-server-tags">' + tags + '</span>' +
            '</span>' +
            '<span class="public-server-action" aria-hidden="true">' + actionIcon + '</span>';

        if (!connected && !pending) {
            btn.addEventListener('click', function() {
                if (match) resumePublicServerInterface(server, match);
                else connectPublicServer(server);
            });
        }

        container.appendChild(btn);
    });
}

function connectPublicServer(server) {
    _connectPendingPublicServerKey = _tcpServerKey(server.host, server.port);
    renderPublicTcpServers(window._hubInterfacesData || window._cachedConfigIfaces || {});
    quickConnect(server.host, server.port, server.name, { publicServer: true });
}

function clearConnectPublicPending() {
    _connectPendingPublicServerKey = null;
    refreshConnectPublicServers();
}

function resumePublicServerInterface(server, match) {
    var iface = match && match.iface ? match.iface : null;
    if (!iface || !iface.name) {
        connectPublicServer(server);
        return;
    }

    _connectPendingPublicServerKey = _tcpServerKey(server.host, server.port);
    renderPublicTcpServers(window._hubInterfacesData || window._cachedConfigIfaces || {});
    RS.invoke('resume_interface', {
        args: {
            name: iface.name,
            iface_type: 'tcp_client'
        }
    }).then(function() {
        if (typeof refreshConfigInterfaces === 'function') refreshConfigInterfaces();
    }).catch(function(err) {
        clearConnectPublicPending();
        showToast((err && err.message) || 'Failed to reconnect', 'toast-red', 8000);
    });
}

function refreshConnectPublicServers(ifaces, opts) {
    opts = opts || {};
    if (ifaces && !opts.force) {
        renderPublicTcpServers(ifaces);
        return;
    }
    if (!opts.force && (window._hubInterfacesData || window._cachedConfigIfaces)) {
        renderPublicTcpServers(window._hubInterfacesData || window._cachedConfigIfaces);
        return;
    }
    RS.invoke('api_hub_interfaces').then(function(data) {
        window._hubInterfacesData = data || {};
        renderPublicTcpServers(data);
    }).catch(function() {
        renderPublicTcpServers({});
    });
}

function _ifaceIfacNetworkName(iface) {
    return _ifaceString(iface, 'network_name', _ifaceString(iface, 'networkname', ''));
}

function _ifaceIfacPassphrase(iface) {
    return _ifaceString(iface, 'passphrase', _ifaceString(iface, 'pass_phrase', ''));
}

function _ifaceHasIfac(iface) {
    return !!(_ifaceIfacNetworkName(iface) || _ifaceIfacPassphrase(iface));
}

function _readConnectIfacValues() {
    if (!_developerModeEnabled()) return null;
    var useIfac = !!(document.getElementById('connect-use-ifac') || {}).checked;
    var networkNameEl = document.getElementById('connect-ifac-network-name');
    var passphraseEl = document.getElementById('connect-ifac-passphrase');
    var networkName = networkNameEl ? networkNameEl.value.trim() : '';
    var passphrase = passphraseEl ? passphraseEl.value.trim() : '';
    return {
        ifac_enabled: useIfac,
        ifac_network_name: useIfac ? networkName : '',
        ifac_passphrase: useIfac ? passphrase : ''
    };
}

function _applyConnectIfacValuesToArgs(args) {
    var ifac = _readConnectIfacValues();
    if (!ifac) return args;
    args.ifac_enabled = ifac.ifac_enabled;
    args.ifac_network_name = ifac.ifac_network_name;
    args.ifac_passphrase = ifac.ifac_passphrase;
    return args;
}

function _syncConnectAdvancedVisibility() {
    var dev = _developerModeEnabled();
    var editContext = _normaliseConnectEditContext(_connectEditContext);
    var isEdit = !!editContext;
    var isBackboneEdit = isEdit && editContext.ifaceType === 'backbone_client';
    var bbRow = document.getElementById('connect-backbone-row');
    var bbCheckbox = document.getElementById('connect-use-backbone');
    if (bbCheckbox) {
        bbCheckbox.disabled = isEdit;
        if (!dev && !isBackboneEdit) bbCheckbox.checked = false;
    }
    if (bbRow) bbRow.style.display = dev ? '' : 'none';

    var ifacRow = document.getElementById('connect-ifac-row');
    var ifacCheckbox = document.getElementById('connect-use-ifac');
    var ifacFields = document.getElementById('connect-ifac-fields');
    var showIfac = dev;
    if (ifacRow) ifacRow.style.display = showIfac ? '' : 'none';
    if (ifacFields) {
        var enabled = showIfac && !!(ifacCheckbox && ifacCheckbox.checked);
        ifacFields.style.display = enabled ? '' : 'none';
    }
}

function openConnectModal(editContext) {
    _connectEditContext = _normaliseConnectEditContext(editContext);
    _connectPendingPublicServerKey = null;
    var iface = _connectEditContext && _connectEditContext.iface ? _connectEditContext.iface : null;
    var isEdit = !!_connectEditContext;
    var isBackboneEdit = isEdit && _connectEditContext.ifaceType === 'backbone_client';
    var titleEl = document.querySelector('#connect-modal .bottom-sheet-title');
    setBottomSheetTitleWithIcon(
        titleEl,
        isEdit ? 'Edit Connection' : 'Connect to Network',
        isBackboneEdit ? 'backbone' : 'tcp'
    );
    document.getElementById('connect-host').value = iface ? _ifaceString(iface, 'target_host', '') : '';
    // Empty so the placeholder shows; submit falls back to 4242.
    document.getElementById('connect-port').value = iface ? _ifaceString(iface, 'target_port', '') : '';
    document.getElementById('connect-name').value = iface ? _ifaceString(iface, 'name', '') : '';
    var tabToggle = document.getElementById('connect-tab-toggle');
    var nameField = document.getElementById('connect-name-field');
    var quickField = document.getElementById('connect-quick-field');
    if (tabToggle) tabToggle.style.display = isEdit ? 'none' : '';
    if (nameField) nameField.style.display = isEdit ? '' : 'none';
    if (quickField) quickField.style.display = isEdit ? 'none' : '';
    setConnectTab(isEdit ? 'custom' : 'public');
    var submitBtn = document.getElementById('connect-submit-btn');
    if (submitBtn) {
        submitBtn.textContent = isEdit ? 'Save Changes' : 'Connect';
        submitBtn.className = 'nr-btn w-full mt-4';
        submitBtn.disabled = false;
    }
    var bbCheckbox = document.getElementById('connect-use-backbone');
    if (bbCheckbox) {
        bbCheckbox.checked = isBackboneEdit;
    }
    var ifacCheckbox = document.getElementById('connect-use-ifac');
    var ifacNetworkName = document.getElementById('connect-ifac-network-name');
    var ifacPassphrase = document.getElementById('connect-ifac-passphrase');
    var hasIfac = _ifaceHasIfac(iface);
    if (ifacCheckbox) ifacCheckbox.checked = hasIfac;
    if (ifacNetworkName) ifacNetworkName.value = iface ? _ifaceIfacNetworkName(iface) : '';
    if (ifacPassphrase) ifacPassphrase.value = iface ? _ifaceIfacPassphrase(iface) : '';
    _syncConnectAdvancedVisibility();
    if (ifacCheckbox && !ifacCheckbox.dataset.bound) {
        ifacCheckbox.dataset.bound = '1';
        ifacCheckbox.addEventListener('change', _syncConnectAdvancedVisibility);
    }
    loadConnectionHistory();
    refreshConnectPublicServers();
    RS.ui.openExistingSheet('connect-modal', 'connect-modal-overlay');
    var body = document.querySelector('#connect-modal .bottom-sheet-body');
    if (body) body.scrollTop = 0;
}

function loadConnectionHistory() {
    var container = document.getElementById('quick-connect-list');
    if (!container) return;

    container.querySelectorAll('.qc-history').forEach(function(el) { el.remove(); });

    var emptyMsg = document.getElementById('qc-empty');

    RS.invoke('api_connection_history').then(function(entries) {
        var customEntries = (entries || []).filter(function(entry) {
            return !_isPublicTcpServer(entry.host, entry.port);
        });

        if (customEntries.length === 0) {
            if (emptyMsg) emptyMsg.style.display = '';
            return;
        }

        if (emptyMsg) emptyMsg.style.display = 'none';

        customEntries.forEach(function(entry) {
            var wrapper = document.createElement('div');
            wrapper.className = 'qc-history';

            var btn = document.createElement('button');
            btn.className = 'quick-connect-btn';
            btn.type = 'button';
            btn.style.flex = '1';
            btn.innerHTML = '<span>' + escapeHtml(entry.name || entry.host) + '</span>' +
                '<span class="quick-connect-detail">' + escapeHtml(entry.host + ':' + entry.port) + '</span>';
            btn.addEventListener('click', function() {
                quickConnect(entry.host, entry.port, entry.name || '');
            });

            var delBtn = document.createElement('button');
            delBtn.className = 'danger-btn-sm';
            delBtn.textContent = '\u00d7';
            delBtn.title = 'Remove from history';
            delBtn.classList.add('qc-delete-btn');
            delBtn.addEventListener('click', function(e) {
                e.stopPropagation();
                RS.invoke('api_delete_connection_history', { id: entry.id }).then(function() {
                    wrapper.remove();
                    if (!container.querySelector('.qc-history') && emptyMsg) {
                        emptyMsg.style.display = '';
                    }
                }).catch(function() {});
            });

            wrapper.appendChild(btn);
            wrapper.appendChild(delBtn);
            container.appendChild(wrapper);
        });
    }).catch(function() {});
}

function closeConnectModal() {
    RS.ui.closeExistingSheet('connect-modal', 'connect-modal-overlay');
    _connectPendingPublicServerKey = null;
    var submitBtn = document.getElementById('connect-submit-btn');
    _setConnectSubmitBase(submitBtn, 'Connect');
    var titleEl = document.querySelector('#connect-modal .bottom-sheet-title');
    setBottomSheetTitleWithIcon(titleEl, 'Connect to Network', 'tcp');
    var bbCheckbox = document.getElementById('connect-use-backbone');
    if (bbCheckbox) bbCheckbox.disabled = false;
    var tabToggle = document.getElementById('connect-tab-toggle');
    var nameField = document.getElementById('connect-name-field');
    var quickField = document.getElementById('connect-quick-field');
    if (tabToggle) tabToggle.style.display = '';
    if (nameField) nameField.style.display = 'none';
    if (quickField) quickField.style.display = '';
    setConnectTab('public');
    _connectEditContext = null;
    _syncConnectAdvancedVisibility();
}

function quickConnect(host, port, name, opts) {
    opts = opts || {};
    _connectEditContext = null;
    var titleEl = document.querySelector('#connect-modal .bottom-sheet-title');
    setBottomSheetTitleWithIcon(titleEl, 'Connect to Network', 'tcp');
    var submitBtn = document.getElementById('connect-submit-btn');
    _setConnectSubmitBase(submitBtn, 'Connect');
    var bbCheckbox = document.getElementById('connect-use-backbone');
    if (bbCheckbox && opts.publicServer) bbCheckbox.checked = false;
    var ifacCheckbox = document.getElementById('connect-use-ifac');
    var ifacNetworkName = document.getElementById('connect-ifac-network-name');
    var ifacPassphrase = document.getElementById('connect-ifac-passphrase');
    if (ifacCheckbox) ifacCheckbox.checked = false;
    if (ifacNetworkName) ifacNetworkName.value = '';
    if (ifacPassphrase) ifacPassphrase.value = '';
    _syncConnectAdvancedVisibility();
    document.getElementById('connect-host').value = host;
    document.getElementById('connect-port').value = port;
    document.getElementById('connect-name').value = name;
    submitConnection();
}

var _connectTimeout = null;

function _clearConnectTimeout() {
    if (_connectTimeout) {
        clearTimeout(_connectTimeout);
        _connectTimeout = null;
    }
}

function _handleConnectInvokeError(err, resetText) {
    _clearConnectTimeout();
    clearConnectPublicPending();
    var btn = document.getElementById('connect-submit-btn');
    if (btn) {
        btn.textContent = 'Failed';
        btn.className = 'nr-btn nr-btn-error w-full mt-4';
        setTimeout(function() {
            _setConnectSubmitBase(btn, resetText || 'Connect');
        }, 3000);
    }
    var message = err && err.message ? err.message : 'Connection request failed';
    showToast(message, 'toast-red', 8000);
}

function _handleInterfaceButtonError(err, buttonId, resetText, fallbackMessage) {
    var btn = document.getElementById(buttonId);
    if (btn) {
        btn.textContent = 'Failed';
        btn.className = 'nr-btn nr-btn-error';
        setTimeout(function() {
            btn.textContent = resetText;
            btn.className = 'nr-btn';
            btn.disabled = false;
        }, 3000);
    }
    showToast((err && err.message) || fallbackMessage || 'Interface request failed', 'toast-red', 8000);
}

function submitConnection() {
    var host = document.getElementById('connect-host').value.trim();
    var port = parseInt(document.getElementById('connect-port').value) || 4242;
    var nameEl = document.getElementById('connect-name');
    var name = nameEl ? nameEl.value.trim() : '';

    if (!host) {
        showPreConditionToast('Please enter a host address');
        return;
    }

    var ifacValues = _readConnectIfacValues();
    if (ifacValues && ifacValues.ifac_enabled &&
        !ifacValues.ifac_network_name && !ifacValues.ifac_passphrase) {
        showPreConditionToast('Enter an IFAC network name or passphrase');
        return;
    }

    var submitBtn = document.getElementById('connect-submit-btn');
    if (submitBtn) {
        submitBtn.textContent = 'Connecting...';
        submitBtn.disabled = true;
    }

    // Recover the button if the backend never acks.
    _clearConnectTimeout();
    _connectTimeout = setTimeout(function() {
        _connectTimeout = null;
        var btn = document.getElementById('connect-submit-btn');
        if (btn && btn.disabled) {
            btn.textContent = 'Timed out';
            btn.className = 'nr-btn nr-btn-error w-full mt-4';
            setTimeout(function() {
                _setConnectSubmitBase(btn, 'Connect');
            }, 3000);
        }
    }, 30000);

    // Backbone checkbox routes to add_backbone_connection (desktop-only).
    var useBackbone = !!(document.getElementById('connect-use-backbone') || {}).checked;
    var editContext = _normaliseConnectEditContext(_connectEditContext);
    _connectEditContext = editContext;
    if (editContext && editContext.ifaceType === 'backbone_client') {
        var bb = editContext.iface || {};
        RS.invoke('update_backbone_connection', {
            args: _applyConnectIfacValuesToArgs({
                old_name: editContext.oldName,
                host: host,
                port: port,
                name: name || ('Backbone to ' + host + ':' + port),
                prefer_ipv6: _ifaceBool(bb, 'prefer_ipv6'),
                connect_timeout: _ifaceInt(bb, 'connect_timeout', null),
                max_reconnect_tries: _ifaceInt(bb, 'max_reconnect_tries', null),
                i2p_tunneled: _ifaceBool(bb, 'i2p_tunneled'),
            })
        }).catch(function(err) { _handleConnectInvokeError(err, 'Save Changes'); });
    } else if (editContext) {
        RS.invoke('update_tcp_connection', {
            args: _applyConnectIfacValuesToArgs({
                old_name: editContext.oldName,
                host: host,
                port: port,
                name: name || (host + ':' + port),
            })
        }).catch(function(err) { _handleConnectInvokeError(err, 'Save Changes'); });
    } else if (useBackbone) {
        RS.invoke('add_backbone_connection', {
            args: _applyConnectIfacValuesToArgs({
                host: host,
                port: port,
                name: name || ('Backbone to ' + host + ':' + port),
            })
        }).catch(function(err) { _handleConnectInvokeError(err, 'Connect'); });
    } else {
        RS.invoke('add_tcp_connection', {
            args: _applyConnectIfacValuesToArgs({
                host: host,
                port: port,
                name: name || (host + ':' + port),
            })
        }).catch(function(err) { _handleConnectInvokeError(err, 'Connect'); });
    }
}

function openHostModal(editContext) {
    _hostEditContext = _normaliseHostEditContext(editContext, 'tcp_server');
    var iface = _hostEditContext && _hostEditContext.iface ? _hostEditContext.iface : null;
    var isEdit = !!_hostEditContext;
    var titleEl = document.querySelector('#host-modal .bottom-sheet-title');
    setBottomSheetTitleWithIcon(titleEl, isEdit ? 'Edit Host' : 'Host Network', 'host');
    document.getElementById('host-port').value = iface ? _ifaceString(iface, 'listen_port', '') : '';
    document.getElementById('host-listen-ip').value = iface ? _ifaceString(iface, 'listen_ip', '0.0.0.0') : '';
    document.getElementById('host-name').value = iface ? _ifaceString(iface, 'name', '') : '';
    var submitBtn = document.getElementById('host-submit-btn');
    if (submitBtn) {
        submitBtn.textContent = isEdit ? 'Save Changes' : 'Start Hosting';
        submitBtn.className = 'nr-btn';
        submitBtn.disabled = false;
    }
    RS.ui.openExistingSheet('host-modal', 'host-modal-overlay');
}

function closeHostModal() {
    RS.ui.closeExistingSheet('host-modal', 'host-modal-overlay');
    var submitBtn = document.getElementById('host-submit-btn');
    if (submitBtn) {
        submitBtn.textContent = 'Start Hosting';
        submitBtn.className = 'nr-btn';
        submitBtn.disabled = false;
    }
    var titleEl = document.querySelector('#host-modal .bottom-sheet-title');
    setBottomSheetTitleWithIcon(titleEl, 'Host Network', 'host');
    _hostEditContext = null;
}

function submitHostServer() {
    var editContext = _normaliseHostEditContext(_hostEditContext, 'tcp_server');
    _hostEditContext = editContext;
    var port = parseInt(document.getElementById('host-port').value) || 4242;
    var listenIp = document.getElementById('host-listen-ip').value.trim() || '0.0.0.0';
    var name = document.getElementById('host-name').value.trim();

    var submitBtn = document.getElementById('host-submit-btn');
    if (submitBtn) {
        submitBtn.textContent = editContext ? 'Saving...' : 'Starting...';
        submitBtn.disabled = true;
    }

    var hostArgs = {
        listen_port: port,
        listen_ip: listenIp,
        name: name || ('TCP Server :' + port),
    };
    if (editContext) hostArgs.old_name = editContext.oldName;
    RS.invoke(editContext ? 'update_tcp_server' : 'add_tcp_server', {
        args: hostArgs
    }).catch(function(err) {
        _handleInterfaceButtonError(
            err,
            'host-submit-btn',
            editContext ? 'Save Changes' : 'Start Hosting',
            'Failed to start TCP server'
        );
    });
}

// Backbone Server is desktop-only; Backbone Client is the checkbox in TCP modal.

function openBackboneHostModal(editContext) {
    _backboneHostEditContext = _normaliseHostEditContext(editContext, 'backbone_server');
    var iface = _backboneHostEditContext && _backboneHostEditContext.iface ? _backboneHostEditContext.iface : null;
    var isEdit = !!_backboneHostEditContext;
    var titleEl = document.querySelector('#backbone-host-modal .bottom-sheet-title');
    setBottomSheetTitleWithIcon(
        titleEl,
        isEdit ? 'Edit Backbone Server' : 'Host Backbone Server',
        'backbone'
    );
    document.getElementById('backbone-host-port').value = iface ? _ifaceString(iface, 'listen_port', '') : '';
    document.getElementById('backbone-host-listen-ip').value = iface ? (_ifaceString(iface, 'listen_on', '') || _ifaceString(iface, 'listen_ip', '0.0.0.0')) : '';
    document.getElementById('backbone-host-name').value = iface ? _ifaceString(iface, 'name', '') : '';
    var submitBtn = document.getElementById('backbone-host-submit-btn');
    if (submitBtn) {
        submitBtn.textContent = isEdit ? 'Save Changes' : 'Start Hosting';
        submitBtn.className = 'nr-btn';
        submitBtn.disabled = false;
    }
    RS.ui.openExistingSheet('backbone-host-modal', 'backbone-host-modal-overlay');
}

function closeBackboneHostModal() {
    RS.ui.closeExistingSheet('backbone-host-modal', 'backbone-host-modal-overlay');
    var submitBtn = document.getElementById('backbone-host-submit-btn');
    if (submitBtn) {
        submitBtn.textContent = 'Start Hosting';
        submitBtn.className = 'nr-btn';
        submitBtn.disabled = false;
    }
    var titleEl = document.querySelector('#backbone-host-modal .bottom-sheet-title');
    setBottomSheetTitleWithIcon(titleEl, 'Host Backbone Server', 'backbone');
    _backboneHostEditContext = null;
}

function submitBackboneHost() {
    var editContext = _normaliseHostEditContext(_backboneHostEditContext, 'backbone_server');
    _backboneHostEditContext = editContext;
    var port = parseInt(document.getElementById('backbone-host-port').value) || 4242;
    var listenIp = document.getElementById('backbone-host-listen-ip').value.trim() || '0.0.0.0';
    var name = document.getElementById('backbone-host-name').value.trim();

    var submitBtn = document.getElementById('backbone-host-submit-btn');
    if (submitBtn) {
        submitBtn.textContent = editContext ? 'Saving...' : 'Starting...';
        submitBtn.disabled = true;
    }

    var args = {
        listen_port: port,
        listen_ip: listenIp,
        name: name || ('Backbone Server :' + port),
    };
    if (editContext) {
        var iface = editContext.iface || {};
        args.old_name = editContext.oldName;
        args.prefer_ipv6 = _ifaceBool(iface, 'prefer_ipv6');
        args.device = _ifaceString(iface, 'device', '') || null;
    }
    RS.invoke(editContext ? 'update_backbone_server' : 'add_backbone_server', {
        args: args
    }).catch(function(err) {
        _handleInterfaceButtonError(
            err,
            'backbone-host-submit-btn',
            editContext ? 'Save Changes' : 'Start Hosting',
            'Failed to start Backbone server'
        );
    });
}

function openInterfaceEditModal(ifaceType, ifaceName, iface) {
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


function toggleLocalNetwork() {
    var isEnabled = !!window._autoEnabled;
    if (isEnabled) {
        rsConfirm({ message: 'Disable Local Network?', danger: true, confirmText: 'Disable' }).then(function(ok) {
            if (ok) RS.invoke('disable_auto_interface', {}).catch(function(err) {
                showToast((err && err.message) || 'Failed to disable Local Network', 'toast-red', 8000);
            });
        });
    } else {
        showAutoInterfaceConfigSheet();
    }
}

// name + group_id always visible; advanced knobs (Python rnsd parity) are
// stripped on mobile to keep the sheet under 75% viewport height.
function showAutoInterfaceConfigSheet() {
    if (typeof _rsBuildSheet !== 'function') return;
    var mobile = (typeof isMobile === 'function') ? isMobile() : false;

    var built = _rsBuildSheet({
        title: 'Local Network',
        titleIcon: interfaceSheetIcon('local'),
        titleIconType: 'local'
    }, function() {});
    built.sheet.classList.add('local-network-sheet');
    built.overlay.addEventListener('click', function(e) {
        if (e.target === built.overlay) built.dismiss(null);
    });

    function makeLabel(text) {
        var l = document.createElement('label');
        l.className = 'rs-dialog-field-label';
        l.textContent = text;
        return l;
    }
    function makeInput(type, value, placeholder) {
        var i = document.createElement('input');
        i.type = type;
        i.className = 'rs-dialog-input';
        if (value !== undefined && value !== null) i.value = value;
        if (placeholder) i.placeholder = placeholder;
        if (type === 'text' && typeof disableAutoCorrect === 'function') {
            disableAutoCorrect(i);
        }
        return i;
    }
    function makeSelect(options, defaultValue) {
        var s = document.createElement('select');
        s.className = 'rs-dialog-input';
        options.forEach(function(opt) {
            var o = document.createElement('option');
            o.value = opt.value;
            o.textContent = opt.label;
            if (opt.value === defaultValue) o.selected = true;
            s.appendChild(o);
        });
        return s;
    }
    function makeHelp(text) {
        var d = document.createElement('div');
        d.className = 'rs-dialog-field-help';
        d.textContent = text;
        return d;
    }

    built.body.appendChild(makeLabel('Name'));
    var nameInput = makeInput('text', 'Local Network', 'Local Network');
    nameInput.maxLength = 64;
    built.body.appendChild(nameInput);
    built.body.appendChild(makeHelp('Display name for this interface.'));

    built.body.appendChild(makeLabel('Group ID'));
    var groupIdInput = makeInput('text', 'reticulum', 'reticulum');
    groupIdInput.maxLength = 63;
    built.body.appendChild(groupIdInput);
    built.body.appendChild(makeHelp('Devices with the same Group ID auto-discover each other; different IDs create isolated subnets on the same physical network.'));

    var details = document.createElement('details');
    details.style.marginTop = 'var(--space-3)';
    var summary = document.createElement('summary');
    summary.className = 'rs-dialog-advanced-summary';
    summary.textContent = 'Advanced';
    details.appendChild(summary);

    var advBody = document.createElement('div');
    advBody.style.paddingTop = 'var(--space-2)';
    details.appendChild(advBody);

    advBody.appendChild(makeLabel('Discovery Scope'));
    var scopeSelect = makeSelect([
        { value: 'link', label: 'Link (default — same Wi-Fi / LAN)' },
        { value: 'admin', label: 'Admin (administrative boundary)' },
        { value: 'site', label: 'Site (cross-router within a site)' },
        { value: 'organisation', label: 'Organisation' },
        { value: 'global', label: 'Global (Internet IPv6 multicast)' }
    ], 'link');
    advBody.appendChild(scopeSelect);
    advBody.appendChild(makeHelp('Most users want Link. Site/Global require IPv6 multicast routing in the upstream network.'));

    var addrTypeSelect, discPortInput, dataPortInput, bitrateInput;
    var nicListContainer = null;
    var nicCheckboxes = [];

    if (!mobile) {
        advBody.appendChild(makeLabel('Multicast Address Type'));
        addrTypeSelect = makeSelect([
            { value: 'temporary', label: 'Temporary (default — RFC 4291 transient)' },
            { value: 'permanent', label: 'Permanent (well-known IANA prefix)' }
        ], 'temporary');
        advBody.appendChild(addrTypeSelect);
        advBody.appendChild(makeHelp('Most users want Temporary. Permanent reserves a stable group address.'));

        advBody.appendChild(makeLabel('Discovery Port'));
        discPortInput = makeInput('number', '29716', '29716');
        discPortInput.min = 1;
        discPortInput.max = 65535;
        advBody.appendChild(discPortInput);

        advBody.appendChild(makeLabel('Data Port'));
        dataPortInput = makeInput('number', '42671', '42671');
        dataPortInput.min = 1;
        dataPortInput.max = 65535;
        advBody.appendChild(dataPortInput);
        advBody.appendChild(makeHelp('Must differ from Discovery Port. Both must be free.'));

        advBody.appendChild(makeLabel('Network Interfaces'));
        nicListContainer = document.createElement('div');
        nicListContainer.style.display = 'flex';
        nicListContainer.style.flexDirection = 'column';
        nicListContainer.style.gap = 'var(--space-1)';
        nicListContainer.style.marginBottom = 'var(--space-2)';
        var loadingNote = document.createElement('div');
        loadingNote.className = 'inline-hint';
        loadingNote.textContent = 'Loading interfaces…';
        nicListContainer.appendChild(loadingNote);
        advBody.appendChild(nicListContainer);
        advBody.appendChild(makeHelp('Leave all unchecked to use all available NICs (excluding loopback + platform-ignored).'));

        RS.invoke('api_list_network_interfaces', {}).then(function(resp) {
            var ifaces = (resp && resp.interfaces) || [];
            nicListContainer.innerHTML = '';
            ifaces.forEach(function(iface) {
                if (iface.is_loopback) return;
                var row = document.createElement('label');
                row.className = 'rs-dialog-checkbox-row';
                var cb = document.createElement('input');
                cb.type = 'checkbox';
                cb.value = iface.name;
                cb.dataset.ifaceName = iface.name;
                row.appendChild(cb);
                var label = document.createElement('span');
                var addr = iface.addr_v6_link_local || iface.addr_v4 || '(no address)';
                label.textContent = iface.name + ' — ' + addr;
                row.appendChild(label);
                nicListContainer.appendChild(row);
                nicCheckboxes.push(cb);
            });
            if (!nicCheckboxes.length) {
                var none = document.createElement('div');
                none.className = 'inline-hint';
                none.textContent = 'No usable network interfaces detected.';
                nicListContainer.appendChild(none);
            }
        }).catch(function() {
            nicListContainer.innerHTML = '';
            var err = document.createElement('div');
            err.className = 'inline-warning';
            err.textContent = 'Could not enumerate interfaces.';
            nicListContainer.appendChild(err);
        });

        advBody.appendChild(makeLabel('Bitrate Override (Mbps, optional)'));
        bitrateInput = makeInput('number', '', '10');
        bitrateInput.min = 1;
        advBody.appendChild(bitrateInput);
        advBody.appendChild(makeHelp('Reported bitrate for transport announce-cap calculation. Default 10 Mbps.'));
    } else {
        // TODO(mobile-advanced): expose multicast type / ports / NICs / bitrate
        // via a sub-sheet if demand surfaces.
    }

    built.body.appendChild(details);

    var cancelBtn = document.createElement('button');
    cancelBtn.className = 'rs-dialog-cancel';
    cancelBtn.textContent = 'Cancel';
    cancelBtn.addEventListener('click', function() { built.dismiss(null); });

    var confirmBtn = document.createElement('button');
    confirmBtn.className = 'rs-dialog-confirm';
    confirmBtn.textContent = 'Enable';
    confirmBtn.addEventListener('click', function() {
        var name = nameInput.value.trim() || 'Local Network';
        var options = {};
        var gid = groupIdInput.value.trim();
        if (gid && gid !== 'reticulum') options.group_id = gid;
        if (scopeSelect.value && scopeSelect.value !== 'link') options.discovery_scope = scopeSelect.value;
        if (!mobile) {
            if (addrTypeSelect.value && addrTypeSelect.value !== 'temporary') {
                options.multicast_address_type = addrTypeSelect.value;
            }
            var dp = parseInt(discPortInput.value, 10);
            if (!isNaN(dp) && dp !== 29716) options.discovery_port = dp;
            var dap = parseInt(dataPortInput.value, 10);
            if (!isNaN(dap) && dap !== 42671) options.data_port = dap;
            var devices = nicCheckboxes.filter(function(c) { return c.checked; }).map(function(c) { return c.value; });
            if (devices.length > 0) options.devices = devices;
            var br = parseFloat(bitrateInput.value);
            if (!isNaN(br) && br > 0) options.configured_bitrate = Math.round(br * 1_000_000);
        }
        built.dismiss({ name: name, options: options });
    });

    built.footer.appendChild(cancelBtn);
    built.footer.appendChild(confirmBtn);

    if (typeof RS !== 'undefined' && RS.gestures && typeof RS.gestures.attachDragDismiss === 'function') {
        RS.gestures.attachDragDismiss(built.sheet, {
            axis: 'y',
            blockIfScrolled: true,
            skipIf: function(e) {
                var t = e.target;
                if (t && (t.tagName === 'INPUT' || t.tagName === 'SELECT' || t.tagName === 'BUTTON' || t.tagName === 'SUMMARY')) return true;
                return false;
            },
            parallaxOverlay: built.overlay,
            onCommit: function() { built.dismiss(null); }
        });
    }

    built.present();

    // present() returns immediately; hook the dismiss callback for results.
    var origDismiss = built.dismiss;
    built.dismiss = function(result) {
        origDismiss(result);
        if (result && result.name) {
            window._activeProgressDialog = rsProgress({
                title: 'Enabling Local Network',
                message: 'Starting...',
                timeout: 15000,
                timeoutMessage: 'Local Network setup timed out. Check your network settings.'
            });
            RS.invoke('enable_auto_interface', { name: result.name, options: result.options }).catch(function(err) {
                if (window._activeProgressDialog && window._activeProgressDialog.error) {
                    window._activeProgressDialog.error((err && err.message) || 'Failed to enable Local Network');
                }
            });
        }
    };
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
        showToast('Requesting Bluetooth permissions\u2026', 'toast-blue', 3000);
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

function toggleBlePeer() {
    RS.invoke('api_ble_peer_available').then(function(data) {
        window._blePeerAvailable = !!data.available;
        updateBlePeerToggle();

        if (!data.available) {
            var missing = data.missing || [];
            var missingStr = missing.join(', ').toLowerCase();

            // Backend can't see Android runtime perms; rescue them here.
            if (hasAndroidBridge() &&
                (missingStr.indexOf('permission') !== -1 || missingStr.indexOf('not initialized') !== -1)) {
                _ensureBlePermissions().then(function(granted) {
                    if (granted) {
                        showToast('Bluetooth permissions granted. Tap + again to enable Bluetooth Peer', 'toast-green', 5000);
                    } else {
                        showToast('Bluetooth permissions denied. Bluetooth Peer requires Bluetooth access', 'toast-red', 5000);
                    }
                });
                return;
            }

            var msg = missing.length > 0
                ? 'Bluetooth Peer is not available: ' + missing.join(', ')
                : 'Bluetooth Peer is not available. No Bluetooth adapter found.';
            showToast(msg, 'toast-orange', 5000);
            return;
        }

        var isEnabled = !!window._blePeerEnabled;
        if (isEnabled) {
            rsConfirm({ message: 'Disable Bluetooth Peer? Active peer connections will be dropped.', danger: true, confirmText: 'Disable' }).then(function(ok) {
                if (ok) RS.invoke('disable_ble_peer_interface').catch(function(err) {
                    showToast((err && err.message) || 'Failed to disable Bluetooth Peer', 'toast-red', 8000);
                });
            });
        } else {
            _ensureBlePermissions().then(function(granted) {
                if (!granted) {
                    showPreConditionToast('Bluetooth Peer requires Bluetooth permissions');
                    return;
                }
                rsChoice({
                    title: 'Enable Bluetooth Peer',
                    titleIcon: interfaceSheetIcon('ble'),
                    titleIconType: 'ble',
                    message: 'Discover nearby Ratspeak users via Bluetooth.\n\nConnect for:',
                    choices: [
                        { label: '10 minutes', value: '600' },
                        { label: '30 minutes', value: '1800' },
                        { label: '60 minutes', value: '3600' },
                        { label: 'Always On', value: '0' }
                    ]
                }).then(function(duration) {
                    if (duration === null) return;
                    window._activeProgressDialog = rsProgress({
                        title: 'Enabling Bluetooth Peer',
                        message: 'Starting...',
                        timeout: 15000,
                        timeoutMessage: 'Bluetooth Peer setup timed out. Check Bluetooth permissions.'
                    });
                    RS.invoke('enable_ble_peer_interface', { args: { duration: parseInt(duration, 10) } }).catch(function(err) {
                        if (window._activeProgressDialog && window._activeProgressDialog.error) {
                            window._activeProgressDialog.error((err && err.message) || 'Failed to enable Bluetooth Peer');
                        } else {
                            showToast((err && err.message) || 'Failed to enable Bluetooth Peer', 'toast-red', 8000);
                        }
                    });
                });
            });
        }
    }).catch(function() {
        showToast('Could not check Bluetooth Peer availability', 'toast-red', 5000);
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

// Cache clears on ble_peer_disconnected echo, not eagerly on click.
function showBlePeerActions(evt, btn) {
    if (evt) { evt.stopPropagation(); evt.preventDefault(); }
    var addressAttr = btn && (btn.getAttribute('data-peer-addresses') || btn.getAttribute('data-peer-address'));
    var addresses = String(addressAttr || '').split(',').map(function(a) { return a.trim(); }).filter(Boolean);
    if (!addresses.length) return;
    function disconnectVisiblePeer() {
        addresses.forEach(function(address) {
            RS.invoke('disconnect_ble_peer', { address: address }).catch(function(err) {
                showToast((err && err.message) || 'Failed to disconnect Bluetooth Peer', 'toast-red', 8000);
            });
        });
    }
    if (typeof rsConfirm === 'function') {
        rsConfirm({
            message: 'Disconnect this peer?\nThe peer can reconnect on the next scan cycle.',
            danger: true,
            confirmText: 'Disconnect',
        }).then(function(ok) {
            if (ok) disconnectVisiblePeer();
        });
    } else {
        disconnectVisiblePeer();
    }
}
window.showBlePeerActions = showBlePeerActions;

document.getElementById('rnode-modal-close').addEventListener('click', closeRnodeModal);
document.getElementById('rnode-refresh-btn').addEventListener('click', refreshRnodeSerialPorts);
document.getElementById('rnode-submit-btn').addEventListener('click', submitRnodeInterface);
document.getElementById('rnode-next-btn').addEventListener('click', rnodeWizardNext);
document.getElementById('rnode-back-btn').addEventListener('click', rnodeWizardBack);
document.getElementById('ble-scan-btn').addEventListener('click', scanBleDevices);

document.getElementById('rnode-port').addEventListener('change', function() {
    var val = this.value;
    if (val) {
        var opt = this.options[this.selectedIndex];
        _selectedSerialPort = { device: val, description: opt ? opt.textContent : val };
    } else {
        _selectedSerialPort = null;
    }
    rnodeUpdateNextBtn();
});

var rnodeRegionSelect = document.getElementById('rnode-region');
if (rnodeRegionSelect) rnodeRegionSelect.addEventListener('change', function() {
    if (this.value !== _RNODE_CUSTOM_REGION_KEY) _rnodeApplyRegionToFrequency(this.value);
    else _rnodeUpdateRadioHints();
});
var rnodePresetSelect = document.getElementById('rnode-preset');
if (rnodePresetSelect) rnodePresetSelect.addEventListener('change', function() {
    if (this.value !== _RNODE_CUSTOM_PRESET_KEY) _rnodeApplyPresetToAdvanced(this.value);
    else {
        var advanced = document.getElementById('rnode-advanced');
        if (advanced) advanced.open = true;
        _rnodeUpdateRadioHints();
    }
});
var rnodeFrequencyInput = document.getElementById('rnode-frequency');
if (rnodeFrequencyInput) rnodeFrequencyInput.addEventListener('input', _rnodeRefreshRegionFromFrequency);
['rnode-bandwidth', 'rnode-spreading-factor', 'rnode-coding-rate', 'rnode-tx-power'].forEach(function(id) {
    var input = document.getElementById(id);
    if (input) input.addEventListener('input', _rnodeRefreshPresetFromAdvanced);
});
var rnodePublicMapEnabled = document.getElementById('rnode-public-map-enabled');
if (rnodePublicMapEnabled) rnodePublicMapEnabled.addEventListener('change', function() {
    if (this.checked) {
        this.checked = false;
        _rnodeEnablePublicMapWithWarning();
    } else {
        _rnodeSetPublicMapEnabled(false);
    }
});
var rnodePublicMapUseCurrent = document.getElementById('rnode-public-map-use-current');
if (rnodePublicMapUseCurrent) rnodePublicMapUseCurrent.addEventListener('click', function() {
    _rnodeSetPublicMapEnabled(true);
    _rnodeRequestPublicMapLocation();
});
['rnode-public-map-latitude', 'rnode-public-map-longitude'].forEach(function(id) {
    var input = document.getElementById(id);
    if (input) input.addEventListener('input', function() {
        _rnodeSetPublicMapError('');
        _rnodeSetPublicMapStatus();
    });
});

var rnodeToggleSerial = document.getElementById('rnode-toggle-serial');
var rnodeToggleBle = document.getElementById('rnode-toggle-ble');
var rnodeToggleAndroidUsb = document.getElementById('rnode-toggle-android-usb');
var rnodeToggleTcp = document.getElementById('rnode-toggle-tcp');
if (rnodeToggleSerial) rnodeToggleSerial.addEventListener('click', function() { setRnodeConnectionType('serial'); });
if (rnodeToggleBle) rnodeToggleBle.addEventListener('click', function() { setRnodeConnectionType('ble'); });
if (rnodeToggleAndroidUsb) rnodeToggleAndroidUsb.addEventListener('click', function() { setRnodeConnectionType('android-usb'); });
if (rnodeToggleTcp) rnodeToggleTcp.addEventListener('click', function() { setRnodeConnectionType('tcp'); });
var rnodeTcpEndpoint = document.getElementById('rnode-tcp-endpoint');
if (rnodeTcpEndpoint) rnodeTcpEndpoint.addEventListener('input', rnodeUpdateNextBtn);
var androidUsbRefresh = document.getElementById('android-usb-refresh-btn');
if (androidUsbRefresh) androidUsbRefresh.addEventListener('click', refreshAndroidUsbDevices);

document.getElementById('rnode-modal').addEventListener('keydown', function(e) {
    if (e.key === 'Enter' && !e.shiftKey && e.target.tagName === 'INPUT') {
        e.preventDefault();
        var step2 = document.getElementById('rnode-step-2');
        var step1 = document.getElementById('rnode-step-1');
        if (step2 && step2.style.display !== 'none') submitRnodeInterface();
        else if (step1 && step1.style.display !== 'none') rnodeWizardNext();
    }
});

document.getElementById('connect-modal-close').addEventListener('click', closeConnectModal);
document.getElementById('connect-submit-btn').addEventListener('click', submitConnection);
document.querySelectorAll('#connect-tab-toggle [data-connect-tab]').forEach(function(btn) {
    btn.addEventListener('click', function() {
        setConnectTab(btn.dataset.connectTab);
    });
});
document.getElementById('connect-modal').addEventListener('keydown', function(e) {
    if (e.key === 'Enter' && !e.shiftKey && e.target.tagName === 'INPUT') {
        e.preventDefault();
        var btn = document.getElementById('connect-submit-btn');
        if (btn && !btn.disabled) submitConnection();
    }
});

document.getElementById('host-modal-close').addEventListener('click', closeHostModal);
document.getElementById('host-submit-btn').addEventListener('click', submitHostServer);
document.getElementById('host-modal').addEventListener('keydown', function(e) {
    if (e.key === 'Enter' && !e.shiftKey && e.target.tagName === 'INPUT') {
        e.preventDefault();
        var btn = document.getElementById('host-submit-btn');
        if (btn && !btn.disabled) submitHostServer();
    }
});

(function() {
    var closeBtn = document.getElementById('backbone-host-modal-close');
    var submitBtn = document.getElementById('backbone-host-submit-btn');
    var modal = document.getElementById('backbone-host-modal');
    if (closeBtn) closeBtn.addEventListener('click', closeBackboneHostModal);
    if (submitBtn) submitBtn.addEventListener('click', submitBackboneHost);
    if (modal) modal.addEventListener('keydown', function(e) {
        if (e.key === 'Enter' && !e.shiftKey && e.target.tagName === 'INPUT') {
            e.preventDefault();
            var btn = document.getElementById('backbone-host-submit-btn');
            if (btn && !btn.disabled) submitBackboneHost();
        }
    });
})();

window._bleAvailable = false;
window._bleMissing = [];
window._bleAuthState = null;
window._blePromptPending = false;
RS.invoke('api_ble_available').then(function(data) {
    window._bleAvailable = !!data.available;
    window._bleMissing = data.missing || [];
    window._bleAuthState = data.auth_state || null;
    window._blePromptPending = data.auth_state === 'not_determined';
    if (data.install_cmd) {
        var cmdEl = document.getElementById('ble-install-cmd');
        if (cmdEl) cmdEl.textContent = data.install_cmd;
    }
}).catch(function() {});

// App-Prefs: scheme is undocumented but stable since iOS 8; WKWebView only
// jumps via location.href.
function openIosBluetoothSettings() {
    try { window.location.href = 'App-Prefs:'; } catch (e) {}
}

[
    { id: 'rnode-modal', overlayId: 'rnode-modal-overlay', closeFn: closeRnodeModal },
    { id: 'connect-modal', overlayId: 'connect-modal-overlay', closeFn: closeConnectModal },
    { id: 'host-modal', overlayId: 'host-modal-overlay', closeFn: closeHostModal },
    { id: 'backbone-host-modal', overlayId: 'backbone-host-modal-overlay', closeFn: closeBackboneHostModal },
    { id: 'node-modal', overlayId: 'node-modal-overlay', closeFn: closeNodeModal },
].forEach(function(cfg) {
    if (typeof initSheetSwipeDismiss === 'function') {
        initSheetSwipeDismiss(cfg.id, cfg.overlayId, cfg.closeFn);
    }
});

document.addEventListener('keydown', function(e) {
    if (e.key === 'Escape') {
        var modals = [
            { id: 'rnode-modal', close: closeRnodeModal },
            { id: 'connect-modal', close: closeConnectModal },
            { id: 'host-modal', close: closeHostModal },
            { id: 'backbone-host-modal', close: closeBackboneHostModal },
            { id: 'node-modal', close: closeNodeModal },
        ];
        for (var i = 0; i < modals.length; i++) {
            var el = document.getElementById(modals[i].id);
            if (el && el.classList.contains('open')) {
                modals[i].close();
                return;
            }
        }
    }
});
