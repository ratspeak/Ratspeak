var announceCache = [];

function isAutoInterfacePeer(iface) {
    var name = (iface && iface.name) || '';
    var typeName = (iface && iface.type) || '';
    return name.indexOf('AutoInterfacePeer[') === 0 || typeName.indexOf('AutoInterfacePeer') >= 0;
}

function isAutoInterfaceAggregate(iface) {
    var name = (iface && iface.name) || '';
    var typeName = (iface && iface.type) || '';
    if (isAutoInterfacePeer(iface)) return false;
    return name.indexOf('AutoInterface[') === 0 || typeName === 'AutoInterface';
}

// A client a server accepted, or a device Local Network found: part of that
// connection, which counts its bytes, not a connection of its own.
function isSpawnedInterface(iface) {
    if (isAutoInterfacePeer(iface)) return true;
    var typeName = (iface && iface.type) || '';
    return !!(iface && iface.parent) && /^(TCPClient|BackboneClient|I2PInterfacePeer|WeaveInterfacePeer)/.test(typeName);
}

function interfaceStatsWithoutAutoPeerDoubleCount(ifaces) {
    var list = Array.isArray(ifaces) ? ifaces : [];
    var hasAutoAggregate = list.some(isAutoInterfaceAggregate);
    var listed = {};
    list.forEach(function(iface) { if (iface && iface.name) listed[iface.name] = true; });
    return list.filter(function(iface) {
        if (isAutoInterfacePeer(iface)) return !hasAutoAggregate;
        return !(isSpawnedInterface(iface) && listed[iface.parent]);
    });
}

function interfaceStatsTotals(ifaces) {
    var totals = { txb: 0, rxb: 0 };
    interfaceStatsWithoutAutoPeerDoubleCount(ifaces).forEach(function(iface) {
        totals.txb += iface.txb || 0;
        totals.rxb += iface.rxb || 0;
    });
    return totals;
}

function renderCockpitStats(stats) {
    var container = document.getElementById('cockpit-stats');
    if (container) {
        var ifaces = stats.interface_stats || [];
        var linkCount = 0, pathCount = 0;
        if (stats.transport) {
            linkCount = stats.transport.link_count || 0;
            pathCount = stats.transport.path_count || 0;
        }
        var totals = interfaceStatsTotals(ifaces);
        container.innerHTML =
            '<div class="cockpit-stat"><span class="cockpit-stat-value">' + linkCount + '</span><span class="cockpit-stat-label">Links</span></div>' +
            '<div class="cockpit-stat"><span class="cockpit-stat-value">' + pathCount + '</span><span class="cockpit-stat-label">Paths</span></div>' +
            '<div class="cockpit-stat"><span class="cockpit-stat-value">' + prettySize(totals.txb) + '</span><span class="cockpit-stat-label">TX</span></div>' +
            '<div class="cockpit-stat"><span class="cockpit-stat-value">' + prettySize(totals.rxb) + '</span><span class="cockpit-stat-label">RX</span></div>';
    }
}

var _lastHomePeersKey = '';
var HOME_PEERS_SHOWN = 6;

function _homeEmpty(tile, tone, title, body, action) {
    return '<div class="ui-empty">' +
        '<span class="ui-tile ui-tile--lg' + (tone ? ' ui-tile--' + tone : '') + '">' + RS.kit.icon(tile) + '</span>' +
        '<h3 class="ui-empty-title">' + title + '</h3>' +
        '<p class="ui-empty-body">' + body + '</p>' +
        '<div class="ui-empty-actions">' + action + '</div></div>';
}

// Home's Recent peers: the most recently heard first, as Peers orders them.
function renderDashboardPeersList() {
    var listEl = document.getElementById('dashboard-peers-list');
    if (!listEl) return;
    if (typeof PeersCache === 'undefined' || !PeersCache) return;

    var peers = PeersCache.enriched();
    peers.sort(PeersCache.compareByLastSeen);
    peers = peers.slice(0, HOME_PEERS_SHOWN);
    // Times move with the clock, so the key includes the minute.
    var key = Math.floor(Date.now() / 60000) + '|' + JSON.stringify(peers);
    if (key === _lastHomePeersKey) return;
    _lastHomePeersKey = key;

    var viewAll = document.getElementById('dash-view-network');
    if (viewAll) viewAll.hidden = peers.length === 0;
    if (peers.length === 0) {
        listEl.innerHTML = _homeEmpty('globe', 'online', 'Listening for peers',
            'People and devices show up here as they announce. Most announce within a few minutes.',
            '<button type="button" class="ui-btn" data-home-announce>' + RS.kit.icon('announce') + 'Announce now</button>');
        return;
    }

    listEl.innerHTML = '<ul class="ui-list">' + peers.map(function(p) {
        var hash = p.hash || '';
        var hasName = p.display_name && p.display_name !== '' && p.display_name !== hash;
        var name = hasName ? p.display_name : RS.kit.hash.short(hash);
        // Under the name: a named peer's short address (an unnamed one shows it as the title), then a Ratspeak status.
        var status = typeof ratspeakProfileStatusText === 'function' ? ratspeakProfileStatusText(p) : '';
        var sub = [hasName ? '<span class="ui-hash">' + escapeHtml(RS.kit.hash.short(hash)) + '</span>' : '', status ? escapeHtml(status) : '']
            .filter(Boolean).join(' · ');
        var heard = p.last_seen ? '<span class="ui-time" title="' + escapeHtml(RS.kit.time.lastHeard(p.last_seen)) + '">' +
            RS.kit.time.row(p.last_seen) + '</span>' : '';
        return '<li><button type="button" class="ui-row" data-hash="' + escapeHtml(hash) + '">' +
            RS.kit.avatar(hash, 'row', RS.kit.rowPresence(p.last_seen), p.last_seen) +
            '<span class="ui-row-main"><span class="ui-row-top"><span class="ui-row-title">' +
                ratspeakDisplayNameHtml(name, p) + '</span>' + heard + '</span>' +
            (sub ? '<span class="ui-row-sub"><span class="t">' + sub + '</span></span>' : '') +
            '</span></button></li>';
    }).join('') + '</ul>';
}

// Home's status card: connected or offline, when this identity last announced, and each connection.
function renderHomeStatus(data) {
    var card = document.getElementById('home-status');
    if (!card) return;
    // A device Local Network found or a client a server accepted is part of that connection, as on Network.
    var ifaces = data && data.interface_stats
        ? interfaceStatsWithoutAutoPeerDoubleCount(data.interface_stats.interfaces).filter(function(iface) { return !isSpawnedInterface(iface); }) : null;
    var html;
    if (!data) {
        html = '<div class="home-status"><span class="ui-tile ui-tile--neutral"><span class="ui-spin"></span></span>' +
            '<div class="home-status-main"><h2 class="ui-card-title" id="home-status-title">Starting the network</h2>' +
            '<p class="home-status-sub">Connections show here once they start.</p></div></div>';
    } else {
        var online = !!data.connected;
        var announced = data.last_announce_at
            ? 'Announced ' + RS.kit.time.sentence(data.last_announce_at) + '.'
            : 'Not announced yet.';
        html = '<div class="home-status"><span class="ui-tile ui-tile--' + (online ? 'online' : 'warning') + '">' +
                RS.kit.icon(online ? 'network' : 'alert') + '</span>' +
            '<div class="home-status-main"><h2 class="ui-card-title" id="home-status-title">' + (online ? 'Connected' : 'Offline') + '</h2>' +
            '<p class="home-status-sub"' + (online && data.last_announce_at ? ' title="' + escapeHtml(RS.kit.time.full(data.last_announce_at)) + '"' : '') + '>' +
                (online ? announced : 'No connection is up. Messages wait on this device and send when one is.') + '</p></div>' +
            (online ? '' : '<button type="button" class="ui-btn ui-btn--primary" data-home-add-connection>Add connection</button>') +
            '</div>';
        var ways = (ifaces || []).map(function(iface) {
            var label = friendlyInterfaceName(iface.name || '', iface.type || '', iface.role || '');
            var kind = typeof personConnectionKind === 'function' ? personConnectionKind(iface.name || '') : '';
            var words = escapeHtml(label) + (kind && kind !== label ? ' · ' + escapeHtml(kind) : '') + (iface.online ? '' : ', offline');
            return '<span class="ui-status"><span class="ui-dot' + (iface.online ? ' ui-dot--online' : '') + '"></span>' + words + '</span>';
        });
        html += '<div class="home-ways">' + ways.join('') +
            '<button type="button" class="ui-link home-open" data-home-open-network>Open Network</button></div>';
    }
    card.innerHTML = html;
    var announce = document.getElementById('dash-announce');
    if (announce) announce.disabled = !!data && !data.connected;
}

function renderDashboardSummaries(data) {
    renderHomeStatus(data);
    renderDashboardPeersList();
}

function getInterfaceAlias(name) {
    try { return localStorage.getItem('iface_alias_' + name); } catch (e) { return null; }
}
function sharedInterfaceLabel(role) {
    if (role === 'shared_instance_peer') return 'Shared instance';
    if (role === 'shared_server') return 'Shared instance server';
    if (role === 'local_client') return 'Local app client';
    return '';
}

// A live interface Join automatically made; the Discovered list names them.
function isJoinedInterfaceName(name) {
    return !!(window.RS && RS.discovery && RS.discovery.isJoined(name));
}

function friendlyInterfaceName(name, typeName, role) {
    var alias = getInterfaceAlias(name);
    if (alias) return alias;
    var sharedLabel = sharedInterfaceLabel(role);
    if (sharedLabel) return sharedLabel;
    if (name.indexOf('AutoInterfacePeer[') === 0) {
        var peerMatch = name.match(/^AutoInterfacePeer\[([^\/\]]+)(?:\/([^\]]+))?\]/);
        if (peerMatch && peerMatch[2]) return peerMatch[1] + ' peer';
        return 'Local peer';
    }
    var clientMatch = name.match(/^TCP to (.+):(\d+)$/);
    if (clientMatch) return clientMatch[1] + ':' + clientMatch[2];
    var serverMatch = name.match(/^TCP Server :(\d+)$/);
    if (serverMatch) return 'Hosting :' + serverMatch[1];
    return name;
}

function interfaceTypeLabel(typeName, role) {
    var sharedLabel = sharedInterfaceLabel(role);
    if (sharedLabel) return sharedLabel;
    return typeName || '';
}

function isTcpInterfaceType(ifaceType) {
    return ifaceType === 'tcp_client' || ifaceType === 'tcp_server'
        || ifaceType === 'backbone_client' || ifaceType === 'backbone_server';
}
function isLoraInterfaceType(ifaceType) {
    return ifaceType === 'rnode';
}
function isEditableInterfaceType(ifaceType) {
    return isLoraInterfaceType(ifaceType) || isTcpInterfaceType(ifaceType);
}
function isInterfaceConfigEnabled(iface) {
    if (!iface || typeof iface !== 'object') return true;
    var enabled = iface.enabled;
    if (enabled === undefined || enabled === null) enabled = iface.interface_enabled;
    if (enabled === undefined || enabled === null) return true;
    return !/^(false|no|0|off)$/i.test(String(enabled).trim());
}
function interfaceSectionForConfigType(ifaceType) {
    if (ifaceType === 'rnode') return 'lora';
    if (ifaceType === 'tcp_client' || ifaceType === 'backbone_client') return 'tcp';
    if (ifaceType === 'tcp_server' || ifaceType === 'backbone_server') return 'host';
    if (ifaceType === 'auto') return 'local';
    return '';
}
function getConfiguredInterfaceRecord(ifaceName) {
    return (_cachedConfigByName && _cachedConfigByName[ifaceName]) || null;
}
function isConfiguredInterfacePaused(ifaceName) {
    var record = getConfiguredInterfaceRecord(ifaceName);
    return !!(record && record.iface && !isInterfaceConfigEnabled(record.iface));
}

// Guidance for BLE RNode failures only a person at the radio or phone can fix.
var BLE_RNODE_HELP = {
    not_reachable: {
        title: 'Radio not found',
        message: 'Ratspeak cannot reach this RNode over Bluetooth. Check that it is powered on and ' +
            'charged (an e-paper screen keeps its last image when the battery is empty), that ' +
            'Bluetooth is enabled, and that no other phone or computer is connected to it. ' +
            'Ratspeak keeps trying. On Android, returning to the app wakes a waiting retry.'
    },
    link_lost: {
        title: 'Connection dropped',
        message: 'The RNode answered, but the Bluetooth link dropped during setup. Keep ' +
            'it powered and close to this device. Ratspeak keeps trying.'
    },
    service_missing: {
        title: 'No RNode service',
        message: 'The device connected but did not offer the RNode Bluetooth service. ' +
            'Check that it runs RNode firmware with Bluetooth enabled.'
    },
    pairing_required: {
        title: 'Pairing needed',
        message: 'Put the RNode in Bluetooth pairing mode, then choose Retry ' +
            'and enter the PIN it shows.'
    },
    stale_bond: {
        title: 'Pair again',
        message: 'The saved Bluetooth pairing did not work. Forget the RNode in Android Bluetooth ' +
            'settings, put the RNode in pairing mode, then choose Retry and enter the ' +
            'PIN it shows.',
        bluetoothSettings: true
    },
    rejected_tx_power: {
        title: 'TX power not supported',
        message: 'The RNode reported a different transmit power than this interface uses, so Ratspeak ' +
            'kept it offline. Choose a TX power supported by this radio and its firmware.',
        editInterface: true
    },
    rejected_settings: {
        title: 'Settings not supported',
        message: 'The RNode did not accept this interface\'s radio settings, so Ratspeak ' +
            'kept it offline. Choose a frequency, bandwidth, spreading factor and coding ' +
            'rate this radio supports.',
        editInterface: true
    },
    unsupported_firmware: {
        title: 'Firmware update needed',
        message: 'This RNode\'s firmware is too old for Ratspeak. Install a supported release using ' +
            'the instructions for your radio, then choose Retry.'
    },
    unsupported_radio: {
        title: 'Radio not supported',
        message: 'This device did not identify itself as a supported RNode. Check that it ' +
            'runs RNode firmware and that this interface uses the right device.'
    },
    radio_unresponsive: {
        title: 'Radio not answering',
        message: 'Ratspeak connected to this RNode, but it did not answer. Ratspeak ' +
            'reconnects on its own; if it keeps happening, restart the RNode.'
    },
    radio_start_failed: {
        title: 'Radio failed to start',
        message: 'The RNode reported that its LoRa radio could not start. Restart it and check that ' +
            'the firmware matches the board. If it keeps failing, follow the radio maker\'s ' +
            'troubleshooting instructions.'
    },
    link_limited: {
        title: 'Bluetooth link too small',
        message: 'The Bluetooth link stayed at its smallest packet size. If the radio does not answer ' +
            'correctly, turn Bluetooth off and on, then choose Retry. Ratspeak ' +
            'requests a larger size during connection setup.'
    },
    pin_rejected: {
        title: 'Pairing not accepted',
        message: 'Bluetooth pairing authentication failed. Put the RNode in pairing mode, then choose ' +
            'Retry and enter the PIN it shows.'
    },
    pairing_window_closed: {
        title: 'Pairing interrupted',
        message: 'Pairing timed out or was cancelled. Put the RNode in pairing mode again, then ' +
            'choose Retry and enter the PIN soon after it shows.'
    },
};

// Wired and network radios need no Bluetooth steps.
var RNODE_PORT_HELP = {
    not_reachable: {
        title: 'Radio not found',
        message: 'Ratspeak cannot reach this RNode. Check that it is plugged in (or ' +
            'reachable on the network) and powered on, and that no other program, such ' +
            'as rnodeconf, is using it. Ratspeak keeps trying.'
    },
    link_lost: {
        title: 'Connection dropped',
        message: 'The RNode answered, but the connection dropped. Check its cable or ' +
            'network connection. Ratspeak keeps trying.'
    },
    port_not_found: {
        title: 'Radio not found',
        message: 'The configured radio port could not be found. Check that the RNode is plugged in, ' +
            'and edit the interface if its port name changed. Ratspeak keeps trying.'
    },
    port_access_denied: {
        title: 'Port access denied',
        message: 'This computer did not let Ratspeak open the radio\'s port. On Linux, ' +
            'add your user to the group that owns it (dialout, or uucp on Arch), then sign ' +
            'out and back in. Ratspeak keeps trying.'
    },
    port_in_use: {
        title: 'Port in use',
        message: 'The radio port could not be opened and may be held by another program. Close any ' +
            'serial monitor, rnodeconf, or other Reticulum instance using it. Ratspeak keeps ' +
            'trying.'
    },
};

function rnodeConnectionHelp(state, reason, port) {
    var retrying = state === 'waiting_for_radio' || state === 'connecting';
    if (retrying && String(port || '').indexOf('ble://') !== 0 && RNODE_PORT_HELP[reason]) {
        return RNODE_PORT_HELP[reason];
    }
    return bleRnodeHelp(state, reason);
}

function bleRnodeHelp(state, reason) {
    var retrying = state === 'waiting_for_radio' || state === 'connecting';
    if (retrying && (reason === 'not_reachable' || reason === 'link_lost' ||
        reason === 'service_missing' || reason === 'radio_unresponsive')) {
        return BLE_RNODE_HELP[reason];
    }
    if ((retrying || state === 'initializing') && reason === 'radio_start_failed') {
        return BLE_RNODE_HELP.radio_start_failed;
    }
    if (state === 'initializing' && reason === 'link_limited') return BLE_RNODE_HELP.link_limited;
    if (state === 'failed' && reason === 'link_limited') return BLE_RNODE_HELP.link_limited;
    if (state === 'failed' && (reason === 'pin_rejected' || reason === 'pairing_window_closed')) {
        return BLE_RNODE_HELP[reason];
    }
    if (state === 'failed' && (reason === 'pairing_required' || reason === 'bond_timeout')) {
        return BLE_RNODE_HELP.pairing_required;
    }
    if (state === 'failed' && reason === 'stale_bond') return BLE_RNODE_HELP.stale_bond;
    if (state === 'failed' && reason === 'rejected_tx_power') return BLE_RNODE_HELP.rejected_tx_power;
    if (state === 'failed' && reason === 'unsupported_firmware') {
        return BLE_RNODE_HELP.unsupported_firmware;
    }
    if (state === 'failed' && /^rejected_/.test(reason)) return BLE_RNODE_HELP.rejected_settings;
    if (state === 'failed' && reason === 'unsupported_radio') return BLE_RNODE_HELP.unsupported_radio;
    return null;
}

function showBleRnodeHelp(help, editInterface) {
    var bridge = typeof hasAndroidBridge === 'function' && hasAndroidBridge()
        ? window.RatspeakAndroid
        : null;
    if (help.editInterface && typeof editInterface === 'function') {
        rsConfirm({
            title: help.title,
            message: help.message,
            confirmText: 'Edit Interface',
            cancelText: 'Close'
        }).then(function(edit) {
            if (edit) editInterface();
        });
        return;
    }
    if (help.bluetoothSettings && bridge && typeof bridge.openBluetoothSettings === 'function') {
        rsConfirm({
            title: help.title,
            message: help.message,
            confirmText: 'Bluetooth Settings',
            cancelText: 'Close'
        }).then(function(open) {
            if (open) {
                try { bridge.openBluetoothSettings(); } catch (_) {}
            }
        });
        return;
    }
    rsAlert({ title: help.title, message: help.message });
}

function requestBleRnodePermission() {
    _ensureBlePermissions().then(function(granted) {
        if (granted) return;
        // Android stops prompting after repeated denials; only Settings can grant.
        var bridge = typeof hasAndroidBridge === 'function' && hasAndroidBridge()
            ? window.RatspeakAndroid
            : null;
        if (!bridge || typeof bridge.openAppPermissionSettings !== 'function') return;
        rsConfirm({
            title: 'Bluetooth permission needed',
            message: 'Allow Nearby devices for Ratspeak in Android settings so it can ' +
                'connect to the RNode.',
            confirmText: 'Open Settings',
            cancelText: 'Close'
        }).then(function(open) {
            if (open) {
                try { bridge.openAppPermissionSettings(); } catch (_) {}
            }
        });
    });
}

function setInterfacePaused(ifaceType, ifaceName, paused) {
    var command = paused ? 'pause_interface' : 'resume_interface';
    var record = getConfiguredInterfaceRecord(ifaceName);
    var iface = record && record.iface;
    var needsUsbPermission = !paused && isAndroid() && iface &&
        String(iface.port || '').indexOf('androidusb://') === 0;

    function invokeLifecycle() { return RS.invoke(command, {
        args: {
            name: ifaceName,
            iface_type: ifaceType
        }
    }); }

    var permission = needsUsbPermission
        ? requestAndroidUsbResumePermission(ifaceName)
        : Promise.resolve(true);
    permission.then(function(granted) {
        if (!granted) return null;
        return invokeLifecycle();
    }).then(function(result) {
        if (result === null) return;
        refreshConfigInterfaces();
    }).catch(function(err) {
        showToast((err && err.message) || 'Could not update interface', 'toast-error', 8000);
    });
}

var _androidUsbResumePermission = null;

function requestAndroidUsbResumePermission(ifaceName) {
    if (!hasAndroidBridge()) {
        return Promise.reject(new Error('Android USB permission recovery is unavailable'));
    }
    if (_androidUsbResumePermission) {
        if (_androidUsbResumePermission.ifaceName === ifaceName) return Promise.resolve(false);
        _androidUsbResumePermission.cancel(new Error('A newer USB reconnect was requested'));
    }
    return new Promise(function(resolve, reject) {
        var finished = false;
        var ownedCallback;
        function finish(error, granted) {
            if (finished) return;
            finished = true;
            clearTimeout(timeout);
            if (window._onUsbSelectorPermissionResult === ownedCallback) {
                window._onUsbSelectorPermissionResult = null;
            }
            if (_androidUsbResumePermission &&
                _androidUsbResumePermission.callback === ownedCallback) {
                _androidUsbResumePermission = null;
            }
            if (error) reject(error);
            else resolve(!!granted);
        }
        var timeout = setTimeout(function() {
            finish(new Error('USB permission request timed out'));
        }, 120000);
        ownedCallback = function(result) {
            if (result && result.granted) {
                finish(null, true);
                return;
            }
            var messages = {
                no_match: 'USB radio is not attached',
                ambiguous: 'More than one matching USB radio is attached',
                selector_mismatch: 'The attached USB radio does not match this connection',
                permission_denied: 'USB permission is required to reconnect the radio',
                service_unavailable: 'Android USB service is unavailable',
                request_failed: 'USB permission request failed'
            };
            var code = result && result.error_code;
            finish(new Error(messages[code] || 'USB permission could not be granted'));
        };
        window._onUsbSelectorPermissionResult = ownedCallback;
        _androidUsbResumePermission = {
            ifaceName: ifaceName,
            callback: ownedCallback,
            cancel: function(error) { finish(error || new Error('USB reconnect cancelled')); }
        };
        RS.invoke('request_android_usb_permission', { name: ifaceName }).catch(function(error) {
            finish(error);
        });
    });
}

function openInterfaceEdit(ifaceType, ifaceName) {
    var record = (_cachedConfigByName && _cachedConfigByName[ifaceName]) || null;
    var iface = record ? record.iface : null;
    if (!iface) {
        showToast('Interface settings are still loading', 'toast-warning', 2500);
        refreshConfigInterfaces();
        return;
    }
    if (typeof openInterfaceEditModal === 'function') {
        openInterfaceEditModal(ifaceType, ifaceName, iface);
    }
}

function renderNetworkOverview(data) {
    if (RS.networkView && typeof RS.networkView.onStats === 'function') RS.networkView.onStats(data);
}

function isUserFacingInterface(iface) {
    var name = iface.name || '';
    var role = iface.role || 'normal';
    if (role === 'local_client' || role === 'shared_instance_peer' || role === 'shared_server') return false;
    if (name.indexOf('SharedInstance') === 0) return false;
    return true;
}

function classifyInterface(iface) {
    var typeName = iface.type || '';
    var role = iface.role || 'normal';
    if (role === 'shared_instance_peer' || role === 'shared_server' || role === 'local_client') return null;
    if (isJoinedInterfaceName(iface.name)) return 'tcp';
    // RNode is always LoRa, regardless of transport (BLE, USB serial).
    // The Bluetooth section is reserved for Bluetooth Peer peering only.
    if (typeName.indexOf('RNode') >= 0) return 'lora';
    if (typeName.indexOf('BlePeer') >= 0) return 'ble';
    if (typeName.indexOf('AutoInterface') >= 0) return 'local';
    // A listener's accepted clients stay with the server that hosts them.
    if (/^(TCPClient|BackboneClient|I2PInterface)/.test(typeName)) return iface.parent ? 'host' : 'tcp';
    if (typeName === 'TCPServerInterface' || typeName === 'BackboneInterface') return 'host';
    var name = (iface.name || '').toLowerCase();
    if (name === 'ble mesh') return 'ble';
    if (name.indexOf('tcp') >= 0 && name.indexOf('server') >= 0) return 'host';
    if (name.indexOf('tcp') >= 0) return 'tcp';
    if (name.indexOf('auto') >= 0) return 'local';
    return null;
}

// Populated from `ble_peer_*` events; raw records stay keyed by BLE address.
// The visible Network section collapses rows by resolved identity because a
// symmetric BLE peering can briefly expose central+peripheral GATT paths for
// the same Ratspeak peer.
window._blePeers = window._blePeers || {};

// Identity-aware label. A BLE address is transport metadata, never a peer
// identity; persisted identity is only a provisional reconnect hint until a
// fresh signed announce makes the row routable.
function _resolveBlePeerLabel(peer) {
    var idHash = peer.identity_hash || '';
    if (idHash) {
        if (typeof PeersCache !== 'undefined' && PeersCache && typeof PeersCache.get === 'function') {
            var entry = PeersCache.get(idHash);
            if (entry && entry.display_name && entry.display_name !== '' && entry.display_name !== idHash) {
                return { label: entry.display_name, title: idHash };
            }
        }
        return { label: typeof shortHash === 'function' ? shortHash(idHash, 8, 4) : idHash.substring(0, 12) + '\u2026', title: idHash };
    }
    var provisional = peer.provisional_identity_hash || '';
    if (provisional && typeof PeersCache !== 'undefined' && PeersCache && typeof PeersCache.get === 'function') {
        var provisionalEntry = PeersCache.get(provisional);
        if (provisionalEntry && provisionalEntry.display_name && provisionalEntry.display_name !== provisional) {
            return { label: 'Verifying ' + provisionalEntry.display_name + '\u2026', title: 'Awaiting a signed identity announce' };
        }
    }
    return { label: 'Identifying peer\u2026', title: 'Awaiting a signed identity announce' };
}

function _blePeerRepresentativeScore(peer) {
    if (!peer) return -1;
    var score = 0;
    if (peer.protocol === 'Ratspeak') score += 1000;
    if (peer.identity_hash) score += 100;
    if (peer.rssi !== undefined && peer.rssi !== null) score += 10;
    return score;
}

function _betterBlePeerRepresentative(current, candidate) {
    if (!current) return candidate;
    var curScore = _blePeerRepresentativeScore(current);
    var nextScore = _blePeerRepresentativeScore(candidate);
    if (nextScore > curScore) return candidate;
    if (nextScore < curScore) return current;
    return (candidate.connected_at || 0) >= (current.connected_at || 0) ? candidate : current;
}

function _bleVisiblePeersFromCache() {
    var raw = Object.keys(window._blePeers || {})
        .map(function(k) { return window._blePeers[k]; })
        .filter(function(p) {
            return p && p.connected === true && p.routable === true && !!p.identity_hash;
        });
    var byIdentity = {};
    var unidentified = [];

    raw.forEach(function(peer) {
        var id = peer.identity_hash || '';
        if (!id) {
            unidentified.push(Object.assign({}, peer, { addresses: [peer.address] }));
            return;
        }
        if (!byIdentity[id]) {
            byIdentity[id] = { peer: null, addresses: [] };
        }
        byIdentity[id].peer = _betterBlePeerRepresentative(byIdentity[id].peer, peer);
        if (peer.address && byIdentity[id].addresses.indexOf(peer.address) === -1) {
            byIdentity[id].addresses.push(peer.address);
        }
    });

    var peers = Object.keys(byIdentity).map(function(id) {
        var group = byIdentity[id];
        var peer = Object.assign({}, group.peer || {});
        peer.identity_hash = id;
        peer.addresses = group.addresses.slice();
        return peer;
    }).concat(unidentified);

    // Ratspeak before Columba; most recent connect first within each.
    peers.sort(function(a, b) {
        var pa = (a.protocol === 'Columba') ? 1 : 0;
        var pb = (b.protocol === 'Columba') ? 1 : 0;
        if (pa !== pb) return pa - pb;
        return (b.connected_at || 0) - (a.connected_at || 0);
    });
    return peers;
}
window._bleVisiblePeersFromCache = _bleVisiblePeersFromCache;

// Refreshed on hub_interfaces_update, reused on stats_update.
var _cachedConfigByName = {};
var _cachedConfigIfaces = null;

function emptyInterfaceConfigPayload() {
    return {
        rnode: [],
        auto: [],
        tcp_client: [],
        tcp_server: [],
        backbone_client: [],
        backbone_server: [],
        mobile_hardware: {},
    };
}

// Row labels shared by Android hardware states and every runtime RNode.
var RNODE_STATE_LABELS = {
    connecting: 'Connecting',
    waiting_for_radio: 'Waiting for radio',
    reconnecting: 'Waiting for radio',
    initializing: 'Initializing',
    disabled: 'Disconnected',
    conflict: 'Radio conflict',
};
var RNODE_FAILED_LABELS = {
    bluetooth_off: 'Bluetooth is off',
    permission_needed: 'Bluetooth permission needed',
    pairing_required: 'Pairing needed',
    bond_timeout: 'Pairing timed out',
    pin_rejected: 'PIN did not match',
    pairing_window_closed: 'Pairing ran out of time',
    stale_bond: 'Pair again',
    bridge_unavailable: 'Radio service unavailable',
    radio_disconnected: 'Waiting for radio',
    auto_resume_disabled: 'Reconnect paused',
    connect_failed: 'Connection failed',
    multiple_configured_radios: 'Radio conflict',
    rejected_tx_power: 'TX power not supported',
    rejected_frequency: 'Frequency not supported',
    rejected_bandwidth: 'Bandwidth not supported',
    rejected_radio_state: 'Radio failed to start',
    rejected_settings: 'Settings not supported',
    unsupported_firmware: 'Firmware update needed',
    unsupported_radio: 'Radio not supported',
};
// Why a retrying radio last failed; unknown reasons stay generic.
var RNODE_RETRY_LABELS = {
    not_reachable: 'Radio not found',
    port_not_found: 'Radio not found',
    port_access_denied: 'Port access denied',
    port_in_use: 'Port in use',
    link_lost: 'Connection dropped',
    service_missing: 'No RNode service',
    bluetooth_off: 'Bluetooth is off',
    permission_needed: 'Bluetooth permission needed',
    radio_unresponsive: 'Radio not answering',
    radio_start_failed: 'Radio failed to start',
    link_limited: 'Bluetooth link too small',
};

function _rnodeStateLabel(state, reason) {
    var retrying = state === 'waiting_for_radio' || state === 'connecting' ||
        state === 'initializing';
    if (state === 'failed') return RNODE_FAILED_LABELS[reason] || 'Connection failed';
    return (retrying && RNODE_RETRY_LABELS[reason]) || RNODE_STATE_LABELS[state] || 'Waiting for radio';
}

// Why a runtime RNode (desktop, Android USB) is not Ready, from live stats.
function _rnodeRuntimeHealth(rnodeState) {
    if (!rnodeState || typeof rnodeState.state !== 'string') return null;
    var reason = typeof rnodeState.reason === 'string' ? rnodeState.reason : '';
    return {
        kind: 'rnode',
        state: rnodeState.state,
        reason: reason,
        label: _rnodeStateLabel(rnodeState.state, reason),
        actionable: rnodeState.state === 'failed',
    };
}

function _mobileRnodeHealth(port, mobileHardware) {
    port = String(port || '');
    mobileHardware = mobileHardware || {};
    var state;
    if (port.indexOf('androidusb://') === 0) {
        state = mobileHardware.usb_rnode || null;
        if (!state) return null;
        var usbLabels = {
            attached: 'USB detected',
            detached: 'USB disconnected',
            permission_granted: 'Reconnecting USB',
            permission_needed: 'USB permission needed',
            inventory_changed: 'Waiting for USB',
        };
        return {
            kind: 'usb_rnode',
            state: state.state,
            reason: state.reason || '',
            label: usbLabels[state.state] || 'Waiting for USB',
            actionable: state.state === 'permission_needed' || state.state === 'detached',
        };
    }
    if (port.indexOf('ble://') === 0) {
        state = mobileHardware.ble_rnode || null;
        if (!state || state.state === 'connected') return null;
        var retrying = state.state === 'waiting_for_radio' || state.state === 'connecting';
        return {
            kind: 'ble_rnode',
            state: state.state,
            reason: state.reason || '',
            label: _rnodeStateLabel(state.state, state.reason || ''),
            actionable: state.state === 'failed' || state.state === 'disabled' ||
                (retrying && state.reason === 'permission_needed'),
        };
    }
    return null;
}

function _configuredInterfaceFallback(record, mobileHardware) {
    var iface = record && record.iface ? record.iface : {};
    var enabled = isInterfaceConfigEnabled(iface);
    var port = String(iface.port || '');
    var waitingForDevice = enabled && port.indexOf('androidusb://') === 0;
    var mobileHealth = enabled
        ? _mobileRnodeHealth(port, mobileHardware)
        : null;
    return {
        paused: !enabled,
        waitingForDevice: waitingForDevice,
        mobileHealth: mobileHealth,
        connecting: enabled && record.ifaceType === 'rnode' && !waitingForDevice && !mobileHealth,
    };
}

function _interfaceConfigByName(ifaces) {
    var byName = {};
    (ifaces.rnode || []).forEach(function(i) { byName[i.name] = { iface: i, ifaceType: 'rnode' }; });
    (ifaces.tcp_client || []).forEach(function(i) { byName[i.name] = { iface: i, ifaceType: 'tcp_client' }; });
    (ifaces.tcp_server || []).forEach(function(i) { byName[i.name] = { iface: i, ifaceType: 'tcp_server' }; });
    (ifaces.backbone_client || []).forEach(function(i) { byName[i.name] = { iface: i, ifaceType: 'backbone_client' }; });
    (ifaces.backbone_server || []).forEach(function(i) { byName[i.name] = { iface: i, ifaceType: 'backbone_server' }; });
    (ifaces.auto || []).forEach(function(i) { byName[i.name] = { iface: i, ifaceType: 'auto' }; });
    return byName;
}

function applyNetworkInterfacePayload(ifaces, opts) {
    opts = opts || {};
    ifaces = ifaces || emptyInterfaceConfigPayload();
    window._hubInterfacesData = ifaces;
    _cachedConfigIfaces = ifaces;
    _cachedConfigByName = _interfaceConfigByName(ifaces);
    if (opts.render !== false) _renderConnectionsFromCache();
    if (typeof refreshConnectPublicServers === 'function') refreshConnectPublicServers(ifaces);
}

function applyMobileHardwareState(data) {
    if (!data || (data.kind !== 'usb_rnode' && data.kind !== 'ble_rnode')) return;
    if (!data.state || typeof data.state !== 'string') return;
    if (!_cachedConfigIfaces) _cachedConfigIfaces = emptyInterfaceConfigPayload();
    if (!_cachedConfigIfaces.mobile_hardware) _cachedConfigIfaces.mobile_hardware = {};
    _cachedConfigIfaces.mobile_hardware[data.kind] = {
        kind: data.kind,
        state: data.state,
        reason: typeof data.reason === 'string' ? data.reason : '',
    };
    if (window._hubInterfacesData) {
        window._hubInterfacesData.mobile_hardware = _cachedConfigIfaces.mobile_hardware;
    }
    _renderConnectionsFromCache();
}

function clearNetworkInterfaceCaches(opts) {
    opts = opts || {};
    var empty = emptyInterfaceConfigPayload();
    window._hubInterfacesData = empty;
    window._hubInterfacesCached = false;
    window._autoEnabled = false;
    window._autoUnavailable = null;
    _cachedConfigIfaces = empty;
    _cachedConfigByName = {};
    lastStats = null;
    _anyInterfaceOnline = null;
    if (opts.render) {
        _renderConnectionsFromCache();
        if (typeof refreshConnectPublicServers === 'function') refreshConnectPublicServers(empty);
        if (typeof updateAutoToggle === 'function') updateAutoToggle();
        if (typeof updateFirstRunInterfaceHintGate === 'function') updateFirstRunInterfaceHintGate(empty);
    }
}

function refreshConfigInterfaces() {
    RS.invoke('api_hub_interfaces').then(function(ifaces) {
        applyNetworkInterfacePayload(ifaces);
    }).catch(function() {});
}

function renderMergedConnections() {
    refreshConfigInterfaces();
}

// Every connection the Network view shows, from live transport stats and the
// saved configuration: { iface, section, ifaceType, paused, waitingForDevice, mobileHealth, connecting }.
function networkConnectionItems() {
    var ifaces = _cachedConfigIfaces;
    var configByName = _cachedConfigByName;
    if (!ifaces) return [];

    // Live transport stats are primary; config data only enriches.
    var allIfaces = [];
    var matchedConfigNames = {};
    if (lastStats && lastStats.interface_stats && lastStats.interface_stats.interfaces) {
        interfaceStatsWithoutAutoPeerDoubleCount(lastStats.interface_stats.interfaces).forEach(function(li) {
            // The connection that spawned it is the row; Turn off there ends it.
            if (!isUserFacingInterface(li) || isSpawnedInterface(li)) return;
            var joined = isJoinedInterfaceName(li.name);
            var configMatch = null;
            // A joined connection is no saved one, whatever saved name its own contains.
            if (!joined) Object.keys(configByName).forEach(function(cn) {
                if (_statsNameMatchesConfig(li.name, cn)) configMatch = configByName[cn];
            });
            if (configMatch && configMatch.iface && configMatch.iface.name) {
                matchedConfigNames[configMatch.iface.name] = true;
            }
            var section = classifyInterface(li);
            // Drivers without a class name fall back to the saved entry.
            if (!section && configMatch) {
                section = interfaceSectionForConfigType(configMatch.ifaceType) || configMatch.ifaceType;
            }
            if (!section) return;
            var ifaceType = configMatch ? configMatch.ifaceType : (joined ? 'joined' : section);
            var paused = !!(configMatch && configMatch.iface && !isInterfaceConfigEnabled(configMatch.iface));
            var renderIface = configMatch && configMatch.iface
                ? Object.assign({}, configMatch.iface, { role: li.role || configMatch.iface.role, type: configMatch.iface.type || li.type })
                : li;
            var mobileHealth = !paused && li.online === false ? _rnodeRuntimeHealth(li.rnode_state) : null;
            allIfaces.push({ iface: renderIface, section: section, ifaceType: ifaceType, paused: paused, mobileHealth: mobileHealth });
        });
    }
    Object.keys(configByName).forEach(function(cn) {
        if (matchedConfigNames[cn]) return;
        var record = configByName[cn];
        if (!record || !record.iface) return;
        var section = interfaceSectionForConfigType(record.ifaceType);
        if (!section) return;
        // Configuration is authoritative for row existence. A newly added
        // interface can precede its first live-statistics snapshot; hiding
        // it until traffic (for example, the first announce) makes a
        // successful add look as if it was discarded.
        var fallback = _configuredInterfaceFallback(record, ifaces.mobile_hardware);
        allIfaces.push({
            iface: record.iface,
            section: section,
            ifaceType: record.ifaceType,
            paused: fallback.paused,
            waitingForDevice: fallback.waitingForDevice,
            mobileHealth: fallback.mobileHealth,
            connecting: fallback.connecting,
        });
    });
    return allIfaces;
}

function _renderConnectionsFromCache() {
    if (RS.networkView && typeof RS.networkView.render === 'function') RS.networkView.render();
}

function confirmRemoveInterface(ifaceType, ifaceName) {
    var msg, confirmText;
    if (ifaceType === 'ble_peer') {
        msg = 'Disable Bluetooth Peer?\nActive peer connections will be dropped.';
        confirmText = 'Disable';
    } else {
        msg = 'Remove "' + ifaceName + '"?';
        confirmText = 'Remove';
    }
    rsConfirm({ message: msg, danger: true, confirmText: confirmText }).then(function(ok) {
        if (ok && typeof removeHubInterface === 'function') removeHubInterface(ifaceType, ifaceName);
    });
}

function renderAlert(alert) {
    var colorClass = alert.level === 'critical' ? 'toast-error' : 'toast-warning';
    var duration = 5000;
    showToast(alert.message, colorClass, duration);
}

RS.listen('announce_received', function(data) {
    announceCache.unshift(data);
    if (announceCache.length > 200) announceCache.length = 200;
});

RS.listen('alert', function(data) {
    renderAlert(data);
    events.push({ type: 'log', category: 'alert', timestamp: Date.now() / 1000, message: '[Alert] ' + (data.message || '') });
    if (events.length > MAX_EVENTS) events.shift();
    renderLog();
});

var _origRenderStats = renderStats;
renderStats = function(data) {
    _origRenderStats(data);
    renderNetworkOverview(data);
    // Re-render from cached config so online status + bytes stay in sync.
    if (isViewActive('network') && typeof _renderConnectionsFromCache === 'function') {
        _renderConnectionsFromCache();
    }
};
