var _modalPreviousFocus = null;

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

    var modal = document.getElementById('node-modal');
    var overlay = document.getElementById('node-modal-overlay');
    modal.classList.add('open');
    if (overlay) overlay.classList.add('active');
    _trapFocus(modal);
}

function closeNodeModal() {
    var modal = document.getElementById('node-modal');
    var overlay = document.getElementById('node-modal-overlay');
    _releaseFocus(modal);
    modal.classList.remove('open');
    if (overlay) overlay.classList.remove('active');
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

var _modalTransportLabels = { auto: 'AUTO', on: 'ON', off: 'OFF' };

function applyModalTransportModePayload(data) {
    var mode = (data && data.mode) || 'off';
    var badge = document.getElementById('modal-transport-select');
    if (badge) {
        badge.textContent = _modalTransportLabels[mode] || mode.toUpperCase();
        badge.setAttribute('data-value', mode);
    }
}

var _modalTransportBadge = document.getElementById('modal-transport-select');
if (_modalTransportBadge) {
    function _openModalTransportChoice() {
        rsChoice({
            title: 'Transport Mode',
            message: 'Relay packets for other nodes on the network.',
            choices: [
                { label: 'AUTO', value: 'auto', hint: 'Enables only on suitable non-LoRa interfaces.' },
                { label: 'ON', value: 'on', hint: 'Always relay packets.' },
                { label: 'OFF', value: 'off', hint: 'Never relay packets.' }
            ]
        }).then(function(mode) {
            if (!mode) return;
            _modalTransportBadge.textContent = _modalTransportLabels[mode] || mode;
            _modalTransportBadge.setAttribute('data-value', mode);

            var networkType = 'unknown';
            if (navigator.connection && navigator.connection.type) {
                networkType = navigator.connection.type;
            } else if (navigator.connection && navigator.connection.effectiveType) {
                networkType = navigator.connection.effectiveType;
            }
            RS.invoke('set_transport_mode', { args: { mode: mode, network_type: networkType } }).catch(function(err) {
                showToast((err && err.message) || 'Failed to update transport mode', 'toast-red', 8000);
            });
        });
    }

    _modalTransportBadge.addEventListener('click', _openModalTransportChoice);
    _modalTransportBadge.addEventListener('keydown', function(e) {
        if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); _openModalTransportChoice(); }
    });
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

function updateHubModalStatusDots() {
    document.querySelectorAll('#node-modal .hub-iface-status').forEach(function(dot) {
        var ifaceName = dot.dataset.ifaceName;
        if (!ifaceName) return;
        var liveData = getInterfaceLiveStatus(ifaceName);
        dot.className = 'hub-iface-status';
        if (liveData) {
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
        var row = document.createElement('div');
        row.className = 'hub-iface-row';

        var statusDot = document.createElement('span');
        statusDot.className = 'hub-iface-status';
        statusDot.dataset.ifaceName = iface.name;
        var liveData = getInterfaceLiveStatus(iface.name);
        if (liveData) {
            statusDot.classList.add(liveData.online ? 'up' : 'down');
            statusDot.title = liveData.online ? 'Connected' : 'Disconnected';
        } else {
            statusDot.classList.add('unknown');
            statusDot.title = 'Waiting for status...';
        }

        var nameSpan = document.createElement('span');
        nameSpan.className = 'hub-iface-name';
        nameSpan.textContent = iface.name;
        nameSpan.title = iface.name;

        var detailSpan = document.createElement('span');
        detailSpan.className = 'hub-iface-detail';
        detailSpan.textContent = getIfaceDetailText(iface, ifaceType);

        var removeBtn = document.createElement('button');
        removeBtn.className = 'danger-btn-sm';
        removeBtn.textContent = 'Remove';
        removeBtn.title = 'Remove this interface';
        removeBtn.addEventListener('click', function() {
            rsConfirm({ message: 'Remove "' + iface.name + '"?', danger: true, confirmText: 'Remove' }).then(function(ok) {
                if (ok) removeHubInterface(ifaceType, iface.name);
            });
        });

        row.appendChild(statusDot);
        row.appendChild(nameSpan);
        row.appendChild(detailSpan);
        row.appendChild(removeBtn);
        container.appendChild(row);
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

function _ifaceBool(iface, key) {
    var v = _ifaceString(iface, key, '').toLowerCase();
    return v === 'true' || v === 'yes' || v === '1';
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
    };
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

function _rnodeModeForPort(port) {
    if ((port || '').indexOf('ble://') === 0) return 'ble';
    if ((port || '').indexOf('androidusb://') === 0) return 'android-usb';
    return 'serial';
}

function openRnodeModal(mode, editIface) {
    mode = mode || 'ble';
    _rnodeEditContext = null;
    // iOS MFi blocks USB serial; Android uses USB-OTG JNI; desktop uses serialport.
    var serialToggle = document.getElementById('rnode-toggle-serial');
    var androidUsbToggle = document.getElementById('rnode-toggle-android-usb');
    if (serialToggle) {
        var hideSerial = isIOS() || (isAndroid() && hasAndroidBridge());
        serialToggle.style.display = hideSerial ? 'none' : '';
    }
    if (androidUsbToggle) {
        androidUsbToggle.style.display = (isAndroid() && hasAndroidBridge()) ? '' : 'none';
    }
    if (isIOS()) mode = 'ble';
    if (isAndroid() && hasAndroidBridge() && mode === 'serial') mode = 'android-usb';
    _rnodeConnectionType = mode;
    _bleSelectedDevice = null;
    _androidUsbSelectedDevice = null;

    var step1 = document.getElementById('rnode-step-1');
    var step2 = document.getElementById('rnode-step-2');
    if (step1) step1.style.display = '';
    if (step2) step2.style.display = 'none';

    var titleEl = document.querySelector('#rnode-modal .bottom-sheet-title');
    if (titleEl) titleEl.textContent = editIface ? 'Edit LoRa Device' : 'Add LoRa Device';
    document.getElementById('rnode-iface-name').value = '';
    var catalogReady = loadRnodePresetCatalog();
    _rnodeApplyDefaultRadioControls();
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
        document.getElementById('rnode-iface-name').value = editIface.name || '';
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
                regionKey === _RNODE_CUSTOM_REGION_KEY || presetKey === _RNODE_CUSTOM_PRESET_KEY
            );
        };
        applyRadioSelection();
        catalogReady.then(applyRadioSelection).catch(function() {});
        var summary = document.getElementById('rnode-device-summary');
        if (summary) summary.textContent = port || 'LoRa radio';
        if (step1) step1.style.display = 'none';
        if (step2) step2.style.display = '';
        var submit = document.getElementById('rnode-submit-btn');
        if (submit) submit.textContent = 'Save Changes';
        rnodeUpdateNextBtn();
    } else {
        var submitBtn = document.getElementById('rnode-submit-btn');
        if (submitBtn) submitBtn.textContent = 'Add Radio';
        catalogReady.then(_rnodeApplyDefaultRadioControls).catch(function() {});
    }

    var modal = document.getElementById('rnode-modal');
    var overlay = document.getElementById('rnode-modal-overlay');
    modal.classList.add('open');
    if (overlay) overlay.classList.add('active');
    _trapFocus(modal);
}

function closeRnodeModal() {
    var modal = document.getElementById('rnode-modal');
    var overlay = document.getElementById('rnode-modal-overlay');
    _releaseFocus(modal);
    modal.classList.remove('open');
    if (overlay) overlay.classList.remove('active');
    _bleSelectedDevice = null;
    _selectedSerialPort = null;
    _androidUsbSelectedDevice = null;
    _rnodeEditContext = null;
    var titleEl = document.querySelector('#rnode-modal .bottom-sheet-title');
    if (titleEl) titleEl.textContent = 'Add LoRa Device';
}

function setRnodeConnectionType(type) {
    _rnodeConnectionType = type;

    var serialBtn = document.getElementById('rnode-toggle-serial');
    var bleBtn = document.getElementById('rnode-toggle-ble');
    var usbBtn = document.getElementById('rnode-toggle-android-usb');
    if (serialBtn) serialBtn.classList.toggle('active', type === 'serial');
    if (bleBtn) bleBtn.classList.toggle('active', type === 'ble');
    if (usbBtn) usbBtn.classList.toggle('active', type === 'android-usb');

    var serialSection = document.getElementById('rnode-serial-section');
    var bleSection = document.getElementById('rnode-ble-section');
    var usbSection = document.getElementById('rnode-android-usb-section');
    if (serialSection) serialSection.style.display = type === 'serial' ? '' : 'none';
    if (bleSection) bleSection.style.display = type === 'ble' ? '' : 'none';
    if (usbSection) usbSection.style.display = type === 'android-usb' ? '' : 'none';

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
    } else {
        hasDevice = !!_selectedSerialPort;
    }
    btn.disabled = !hasDevice;
}

function rnodeWizardNext() {
    var step1 = document.getElementById('rnode-step-1');
    var step2 = document.getElementById('rnode-step-2');
    if (!step1 || !step2) return;

    var summary = document.getElementById('rnode-device-summary');
    var nameInput = document.getElementById('rnode-iface-name');

    if (_rnodeConnectionType === 'ble' && _bleSelectedDevice) {
        summary.textContent = _bleSelectedDevice.name + ' via Bluetooth';
        if (!nameInput.value.trim()) nameInput.value = _bleSelectedDevice.name || 'LoRa Radio';
    } else if (_rnodeConnectionType === 'android-usb' && _androidUsbSelectedDevice) {
        var usbLabel = _androidUsbSelectedDevice.product
            || _androidUsbSelectedDevice.manufacturer
            || _androidUsbSelectedDevice.device_name;
        summary.textContent = usbLabel + ' via USB';
        if (!nameInput.value.trim()) nameInput.value = 'LoRa Radio';
    } else if (_selectedSerialPort) {
        var desc = _selectedSerialPort.description || _selectedSerialPort.device;
        summary.textContent = desc + ' via USB';
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
        window.RatspeakAndroid.requestBlePermissions();
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
        window.RatspeakAndroid.scanBleDevices(5000);
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

    // By this point the device is either bonded or non-BLE.
    var chain = Promise.resolve(true);

    chain.then(function(ok) {
        if (!ok) return;

        // Prompt for USB permission before Rust opens the device.
        var proceed = Promise.resolve(true);
        if (_rnodeConnectionType === 'android-usb' && _androidUsbSelectedDevice && hasAndroidBridge()) {
            var devName = _androidUsbSelectedDevice.device_name;
            if (window.RatspeakAndroid.hasUsbPermission(devName)) {
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
                frequency: radioSettings.frequency,
                bandwidth: radioSettings.bandwidth,
                spreading_factor: radioSettings.spreadingFactor,
                coding_rate: radioSettings.codingRate,
                tx_power: radioSettings.txPower,
            };
            if (radioSettings.regionKey !== _RNODE_CUSTOM_REGION_KEY) loraArgs.region_key = radioSettings.regionKey;
            if (radioSettings.presetKey !== _RNODE_CUSTOM_PRESET_KEY) loraArgs.preset_key = radioSettings.presetKey;
            if (radioSettings.customParams) loraArgs.custom_params = true;
            if (isEdit) loraArgs.old_name = _rnodeEditContext.oldName;
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
            }
            loraRequest.catch(function(err) {
                if (window._activeProgressDialog && window._activeProgressDialog.error) {
                    window._activeProgressDialog.error((err && err.message) || 'Failed to configure LoRa interface');
                } else {
                    showToast((err && err.message) || 'Failed to configure LoRa interface', 'toast-red', 8000);
                }
            });
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

function openConnectModal(editContext) {
    _connectEditContext = _normaliseConnectEditContext(editContext);
    var iface = _connectEditContext && _connectEditContext.iface ? _connectEditContext.iface : null;
    var isEdit = !!_connectEditContext;
    var isBackboneEdit = isEdit && _connectEditContext.ifaceType === 'backbone_client';
    var titleEl = document.querySelector('#connect-modal .bottom-sheet-title');
    if (titleEl) titleEl.textContent = isEdit ? 'Edit Connection' : 'Connect to Network';
    document.getElementById('connect-host').value = iface ? _ifaceString(iface, 'target_host', '') : '';
    // Empty so the placeholder shows; submit falls back to 4242.
    document.getElementById('connect-port').value = iface ? _ifaceString(iface, 'target_port', '') : '';
    document.getElementById('connect-name').value = iface ? _ifaceString(iface, 'name', '') : '';
    var submitBtn = document.getElementById('connect-submit-btn');
    if (submitBtn) {
        submitBtn.textContent = isEdit ? 'Save Changes' : 'Connect';
        submitBtn.className = 'nr-btn';
        submitBtn.disabled = false;
    }
    // Backbone toggle is desktop-only, off by default.
    var bbRow = document.getElementById('connect-backbone-row');
    var bbCheckbox = document.getElementById('connect-use-backbone');
    if (bbCheckbox) {
        bbCheckbox.checked = isBackboneEdit;
        bbCheckbox.disabled = isEdit;
    }
    if (bbRow) {
        var isDesktop = typeof window !== 'undefined' && !!window.__RATSPEAK_DESKTOP__;
        bbRow.style.display = (isDesktop || isBackboneEdit) ? '' : 'none';
    }
    loadConnectionHistory();
    var modal = document.getElementById('connect-modal');
    var overlay = document.getElementById('connect-modal-overlay');
    modal.classList.add('open');
    if (overlay) overlay.classList.add('active');
    _trapFocus(modal);
}

function loadConnectionHistory() {
    var container = document.getElementById('quick-connect-list');
    if (!container) return;

    container.querySelectorAll('.qc-history').forEach(function(el) { el.remove(); });

    var emptyMsg = document.getElementById('qc-empty');

    RS.invoke('api_connection_history').then(function(entries) {
        if (!entries || entries.length === 0) {
            if (emptyMsg) emptyMsg.style.display = '';
            return;
        }

        if (emptyMsg) emptyMsg.style.display = 'none';

        entries.forEach(function(entry) {
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
    var modal = document.getElementById('connect-modal');
    var overlay = document.getElementById('connect-modal-overlay');
    _releaseFocus(modal);
    modal.classList.remove('open');
    if (overlay) overlay.classList.remove('active');
    var submitBtn = document.getElementById('connect-submit-btn');
    if (submitBtn) {
        submitBtn.textContent = 'Connect';
        submitBtn.className = 'nr-btn';
        submitBtn.disabled = false;
    }
    var titleEl = document.querySelector('#connect-modal .bottom-sheet-title');
    if (titleEl) titleEl.textContent = 'Connect to Network';
    var bbCheckbox = document.getElementById('connect-use-backbone');
    if (bbCheckbox) bbCheckbox.disabled = false;
    _connectEditContext = null;
}

function quickConnect(host, port, name) {
    _connectEditContext = null;
    var titleEl = document.querySelector('#connect-modal .bottom-sheet-title');
    if (titleEl) titleEl.textContent = 'Connect to Network';
    var submitBtn = document.getElementById('connect-submit-btn');
    if (submitBtn) {
        submitBtn.textContent = 'Connect';
        submitBtn.className = 'nr-btn';
        submitBtn.disabled = false;
    }
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
    var btn = document.getElementById('connect-submit-btn');
    if (btn) {
        btn.textContent = 'Failed';
        btn.className = 'nr-btn nr-btn-error';
        setTimeout(function() {
            btn.textContent = resetText || 'Connect';
            btn.className = 'nr-btn';
            btn.disabled = false;
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
    var name = document.getElementById('connect-name').value.trim();

    if (!host) {
        showPreConditionToast('Please enter a host address');
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
            btn.className = 'nr-btn nr-btn-error';
            setTimeout(function() {
                btn.textContent = 'Connect';
                btn.className = 'nr-btn';
                btn.disabled = false;
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
            args: {
                old_name: editContext.oldName,
                host: host,
                port: port,
                name: name || ('Backbone to ' + host + ':' + port),
                prefer_ipv6: _ifaceBool(bb, 'prefer_ipv6'),
                connect_timeout: _ifaceInt(bb, 'connect_timeout', null),
                max_reconnect_tries: _ifaceInt(bb, 'max_reconnect_tries', null),
                i2p_tunneled: _ifaceBool(bb, 'i2p_tunneled'),
            }
        }).catch(function(err) { _handleConnectInvokeError(err, 'Save Changes'); });
    } else if (editContext) {
        RS.invoke('update_tcp_connection', {
            args: {
                old_name: editContext.oldName,
                host: host,
                port: port,
                name: name || ('TCP to ' + host + ':' + port),
            }
        }).catch(function(err) { _handleConnectInvokeError(err, 'Save Changes'); });
    } else if (useBackbone) {
        RS.invoke('add_backbone_connection', {
            args: {
                host: host,
                port: port,
                name: name || ('Backbone to ' + host + ':' + port),
            }
        }).catch(function(err) { _handleConnectInvokeError(err, 'Connect'); });
    } else {
        RS.invoke('add_tcp_connection', {
            args: {
                host: host,
                port: port,
                name: name || ('TCP to ' + host + ':' + port),
            }
        }).catch(function(err) { _handleConnectInvokeError(err, 'Connect'); });
    }
}

function openHostModal(editContext) {
    _hostEditContext = _normaliseHostEditContext(editContext, 'tcp_server');
    var iface = _hostEditContext && _hostEditContext.iface ? _hostEditContext.iface : null;
    var isEdit = !!_hostEditContext;
    var titleEl = document.querySelector('#host-modal .bottom-sheet-title');
    if (titleEl) titleEl.textContent = isEdit ? 'Edit Host' : 'Host Network';
    document.getElementById('host-port').value = iface ? _ifaceString(iface, 'listen_port', '') : '';
    document.getElementById('host-listen-ip').value = iface ? _ifaceString(iface, 'listen_ip', '0.0.0.0') : '';
    document.getElementById('host-name').value = iface ? _ifaceString(iface, 'name', '') : '';
    var submitBtn = document.getElementById('host-submit-btn');
    if (submitBtn) {
        submitBtn.textContent = isEdit ? 'Save Changes' : 'Start Hosting';
        submitBtn.className = 'nr-btn';
        submitBtn.disabled = false;
    }
    var modal = document.getElementById('host-modal');
    var overlay = document.getElementById('host-modal-overlay');
    modal.classList.add('open');
    if (overlay) overlay.classList.add('active');
    _trapFocus(modal);
}

function closeHostModal() {
    var modal = document.getElementById('host-modal');
    var overlay = document.getElementById('host-modal-overlay');
    _releaseFocus(modal);
    modal.classList.remove('open');
    if (overlay) overlay.classList.remove('active');
    var submitBtn = document.getElementById('host-submit-btn');
    if (submitBtn) {
        submitBtn.textContent = 'Start Hosting';
        submitBtn.className = 'nr-btn';
        submitBtn.disabled = false;
    }
    var titleEl = document.querySelector('#host-modal .bottom-sheet-title');
    if (titleEl) titleEl.textContent = 'Host Network';
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
    if (titleEl) titleEl.textContent = isEdit ? 'Edit Backbone Server' : 'Host Backbone Server';
    document.getElementById('backbone-host-port').value = iface ? _ifaceString(iface, 'listen_port', '') : '';
    document.getElementById('backbone-host-listen-ip').value = iface ? (_ifaceString(iface, 'listen_on', '') || _ifaceString(iface, 'listen_ip', '0.0.0.0')) : '';
    document.getElementById('backbone-host-name').value = iface ? _ifaceString(iface, 'name', '') : '';
    var submitBtn = document.getElementById('backbone-host-submit-btn');
    if (submitBtn) {
        submitBtn.textContent = isEdit ? 'Save Changes' : 'Start Hosting';
        submitBtn.className = 'nr-btn';
        submitBtn.disabled = false;
    }
    var modal = document.getElementById('backbone-host-modal');
    var overlay = document.getElementById('backbone-host-modal-overlay');
    modal.classList.add('open');
    if (overlay) overlay.classList.add('active');
    _trapFocus(modal);
}

function closeBackboneHostModal() {
    var modal = document.getElementById('backbone-host-modal');
    var overlay = document.getElementById('backbone-host-modal-overlay');
    _releaseFocus(modal);
    modal.classList.remove('open');
    if (overlay) overlay.classList.remove('active');
    var submitBtn = document.getElementById('backbone-host-submit-btn');
    if (submitBtn) {
        submitBtn.textContent = 'Start Hosting';
        submitBtn.className = 'nr-btn';
        submitBtn.disabled = false;
    }
    var titleEl = document.querySelector('#backbone-host-modal .bottom-sheet-title');
    if (titleEl) titleEl.textContent = 'Host Backbone Server';
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

    var built = _rsBuildSheet({ title: 'Local Network' }, function() {});
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
    var address = btn && btn.getAttribute('data-peer-address');
    if (!address) return;
    if (typeof rsConfirm === 'function') {
        rsConfirm({
            message: 'Disconnect this peer?\nThe peer can reconnect on the next scan cycle.',
            danger: true,
            confirmText: 'Disconnect',
        }).then(function(ok) {
            if (ok) RS.invoke('disconnect_ble_peer', { address: address }).catch(function(err) {
                showToast((err && err.message) || 'Failed to disconnect Bluetooth Peer', 'toast-red', 8000);
            });
        });
    } else {
        RS.invoke('disconnect_ble_peer', { address: address }).catch(function(err) {
            showToast((err && err.message) || 'Failed to disconnect Bluetooth Peer', 'toast-red', 8000);
        });
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

var rnodeToggleSerial = document.getElementById('rnode-toggle-serial');
var rnodeToggleBle = document.getElementById('rnode-toggle-ble');
var rnodeToggleAndroidUsb = document.getElementById('rnode-toggle-android-usb');
if (rnodeToggleSerial) rnodeToggleSerial.addEventListener('click', function() { setRnodeConnectionType('serial'); });
if (rnodeToggleBle) rnodeToggleBle.addEventListener('click', function() { setRnodeConnectionType('ble'); });
if (rnodeToggleAndroidUsb) rnodeToggleAndroidUsb.addEventListener('click', function() { setRnodeConnectionType('android-usb'); });
var androidUsbRefresh = document.getElementById('android-usb-refresh-btn');
if (androidUsbRefresh) androidUsbRefresh.addEventListener('click', refreshAndroidUsbDevices);

document.getElementById('rnode-modal').addEventListener('keydown', function(e) {
    if (e.key === 'Enter' && !e.shiftKey && e.target.tagName === 'INPUT') {
        e.preventDefault();
        var step2 = document.getElementById('rnode-step-2');
        if (step2 && step2.style.display !== 'none') submitRnodeInterface();
    }
});

document.getElementById('connect-modal-close').addEventListener('click', closeConnectModal);
document.getElementById('connect-submit-btn').addEventListener('click', submitConnection);
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
