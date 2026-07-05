var announceCache = [];
var interfaceHistory = {};  // name -> [{t, txb, rxb}, ...]

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

function interfaceStatsWithoutAutoPeerDoubleCount(ifaces) {
    var list = Array.isArray(ifaces) ? ifaces : [];
    var hasAutoAggregate = list.some(isAutoInterfaceAggregate);
    if (!hasAutoAggregate) return list;
    return list.filter(function(iface) { return !isAutoInterfacePeer(iface); });
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

var _lastPeersJson = '';

function renderDashboardPeersList() {
    var listEl = document.getElementById('dashboard-peers-list');
    if (!listEl) return;
    if (typeof PeersCache === 'undefined' || !PeersCache) return;

    var peers = PeersCache.enriched();
    var peersJson = JSON.stringify(peers);
    if (peersJson === _lastPeersJson) return;
    _lastPeersJson = peersJson;

    if (peers.length === 0) {
        listEl.innerHTML = '<div class="dashboard-peers-empty">No peers discovered yet</div>';
        return;
    }

    // Reachable first, then stale, then offline; within a group, by name.
    var SP = { reachable: 0, direct: 0, stale: 1, offline: 2, unreachable: 3, unknown: 4 };
    peers.sort(function(a, b) {
        var sa = SP[a.status] !== undefined ? SP[a.status] : 3;
        var sb = SP[b.status] !== undefined ? SP[b.status] : 3;
        if (sa !== sb) return sa - sb;
        return (a.display_name || a.hash || '').localeCompare(b.display_name || b.hash || '');
    });

    var MAX_DISPLAY = 50;
    var html = '';
    var msgIcon = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/></svg>';
    var addIcon = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M16 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2"/><circle cx="8.5" cy="7" r="4"/><line x1="20" y1="8" x2="20" y2="14"/><line x1="23" y1="11" x2="17" y2="11"/></svg>';

    peers.slice(0, MAX_DISPLAY).forEach(function(p) {
        var hash = p.hash || '';
        var statusClass = 'status-' + p.status;
        var av = (typeof identityAvatar === 'function') ? identityAvatar(hash, 24) : '';
        var hasName = p.display_name && p.display_name !== '' && p.display_name !== hash;
        var displayName = hasName ? p.display_name : hash.substring(0, 8);
        var nameClass = 'dashboard-peers-name' + (hasName ? '' : ' dashboard-peers-name-hash');
        var profileStatus = typeof ratspeakProfileStatusText === 'function' ? ratspeakProfileStatusText(p) : '';
        var statusHtml = profileStatus
            ? '<span class="dashboard-peers-status" title="' + escapeHtml(profileStatus) + '">' + escapeHtml(profileStatus) + '</span>'
            : '';
        var hopText = (p.hops !== null && p.hops !== undefined)
            ? p.hops + (p.hops === 1 ? ' hop' : ' hops') : '';

        var actions = '<div class="dashboard-peers-actions">';
        actions += '<button class="dashboard-peers-action-btn" data-action="message" data-hash="' + escapeHtml(hash) + '" title="Message">' + msgIcon + '</button>';
        if (!p.is_contact) {
            actions += '<button class="dashboard-peers-action-btn" data-action="add-contact" data-hash="' + escapeHtml(hash) + '" title="Add Contact">' + addIcon + '</button>';
        }
        actions += '</div>';

        html += '<div class="dashboard-peers-row' + (profileStatus ? ' has-profile-status' : '') + '" data-hash="' + escapeHtml(hash) + '">' +
            '<span class="dashboard-peers-dot ' + statusClass + '"></span>' +
            '<div class="dashboard-peers-avatar">' + av + '</div>' +
            '<span class="dashboard-peers-main">' +
                '<span class="' + nameClass + '">' + ratspeakDisplayNameHtml(displayName, p) + '</span>' +
                statusHtml +
            '</span>' +
            (hopText ? '<span class="dashboard-peers-hops">' + hopText + '</span>' : '') +
            actions +
            '</div>';
    });

    if (peers.length > MAX_DISPLAY) {
        html += '<div class="dashboard-peers-empty text-xs" style="padding:var(--space-4) var(--space-7);">Showing ' + MAX_DISPLAY + ' of ' + peers.length + ' peers &mdash; <a id="dash-peers-view-all" class="text-accent" style="cursor:pointer;">View all</a></div>';
    }

    listEl.innerHTML = html;

    var viewAllLink = document.getElementById('dash-peers-view-all');
    if (viewAllLink) {
        viewAllLink.addEventListener('click', function() {
            if (typeof switchView === 'function') switchView('peers');
        });
    }

    listEl.querySelectorAll('.dashboard-peers-row').forEach(function(row) {
        row.addEventListener('click', function(e) {
            if (e.target.closest('.dashboard-peers-action-btn')) return;
            var h = this.dataset.hash;
            if (typeof openConversationWith === 'function') openConversationWith(h);
        });
    });
    listEl.querySelectorAll('[data-action="message"]').forEach(function(btn) {
        btn.addEventListener('click', function(e) {
            e.stopPropagation();
            if (typeof openConversationWith === 'function') openConversationWith(this.dataset.hash);
        });
    });
    listEl.querySelectorAll('[data-action="add-contact"]').forEach(function(btn) {
        btn.addEventListener('click', function(e) {
            e.stopPropagation();
            var h = this.dataset.hash;
            var cd = null;
            (PeersCache && PeersCache.enriched() || []).forEach(function(c) { if (c.hash === h) cd = c; });
            var prefill = cd ? (cd.display_name || '') : '';
            rsPrompt({ message: 'Contact name (optional):', placeholder: 'Display name', defaultValue: prefill }).then(function(name) {
                if (name === null) return;
                RS.invokeOrToast('add_contact', { args: { hash: h, display_name: name.trim() || null } }, 'Could not add contact');
            });
        });
    });
}

function renderDashboardSummaries(data) {
    renderDashboardPeersList();
    if (typeof renderNetworkContactList === 'function') renderNetworkContactList();
}

function toggleSubPanel(id) {
    var panel = document.getElementById(id);
    if (panel) panel.classList.toggle('collapsed');
}

document.addEventListener('DOMContentLoaded', function() {
    var pathToggle = document.getElementById('toggle-subpanel-paths');
    if (pathToggle) pathToggle.addEventListener('click', function() { toggleSubPanel('subpanel-paths'); });
    var announceToggle = document.getElementById('toggle-subpanel-announces');
    if (announceToggle) announceToggle.addEventListener('click', function() { toggleSubPanel('subpanel-announces'); });
});

function getInterfaceAlias(name) {
    try { return localStorage.getItem('iface_alias_' + name); } catch (e) { return null; }
}
function setInterfaceAlias(name, alias) {
    try { localStorage.setItem('iface_alias_' + name, alias); } catch (e) {}
}
function clearInterfaceAlias(name) {
    try { localStorage.removeItem('iface_alias_' + name); } catch (e) {}
}

// Section collapse: per-section, session-scoped. Default = section with a
// running interface starts open. User toggles win within the session
// (sessionStorage clears on WebView restart, default rule re-applies).
function getConnSectionPref(key) {
    try { return sessionStorage.getItem('conn_section_' + key + '_state'); } catch (e) { return null; }
}
function setConnSectionPref(key, state) {
    try { sessionStorage.setItem('conn_section_' + key + '_state', state); } catch (e) {}
}
function shouldSectionBeCollapsed(key, hasInterfaces) {
    var pref = getConnSectionPref(key);
    if (pref === 'closed') return true;
    if (pref === 'open') return false;
    return !hasInterfaces;
}

function sharedInterfaceLabel(role) {
    if (role === 'shared_instance_peer') return 'Shared instance';
    if (role === 'shared_server') return 'Shared instance server';
    if (role === 'local_client') return 'Local app client';
    return '';
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

function openRenameInterfaceDialog(ifaceName) {
    var current = getInterfaceAlias(ifaceName) || '';
    rsPrompt({
        message: 'Rename interface',
        placeholder: 'Display name (leave blank to reset)',
        defaultValue: current
    }).then(function(newName) {
        if (newName === null) return;
        var trimmed = newName.trim();
        if (trimmed) setInterfaceAlias(ifaceName, trimmed);
        else clearInterfaceAlias(ifaceName);
        if (typeof renderConnections === 'function') renderConnections();
        else if (typeof _renderConnectionsFromCache === 'function') _renderConnectionsFromCache();
    });
}

var ICON_PENCIL = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M17 3a2.85 2.85 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5Z"/><path d="m15 5 4 4"/></svg>';
var ICON_RADIO = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="2"/><path d="M16.24 7.76a6 6 0 0 1 0 8.49m-8.48-.01a6 6 0 0 1 0-8.49m11.31-2.82a10 10 0 0 1 0 14.14m-14.14 0a10 10 0 0 1 0-14.14"/></svg>';
var ICON_TRASH = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="3 6 5 6 21 6"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/></svg>';
var ICON_PAUSE = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="6" y="4" width="4" height="16" rx="1"/><rect x="14" y="4" width="4" height="16" rx="1"/></svg>';
var ICON_PLAY = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polygon points="6 3 20 12 6 21 6 3"/></svg>';

function buildIfaceActionItems(ifaceType, ifaceName) {
    var items = [];
    var record = getConfiguredInterfaceRecord(ifaceName);
    var supportsPause = !!(record && record.iface && ifaceType !== 'ble_peer');
    var paused = supportsPause && !isInterfaceConfigEnabled(record.iface);
    if (supportsPause) {
        items.push({
            label: paused ? 'Resume Interface' : 'Pause Interface',
            icon: paused ? ICON_PLAY : ICON_PAUSE,
            onSelect: function() { setInterfacePaused(ifaceType, ifaceName, !paused); }
        });
        items.push({ separator: true });
    }
    if (isLoraInterfaceType(ifaceType)) {
        items.push({ label: 'Edit', icon: ICON_RADIO, onSelect: function() { openInterfaceEdit(ifaceType, ifaceName); } });
    } else if (isTcpInterfaceType(ifaceType)) {
        items.push({ label: 'Edit', icon: ICON_PENCIL, onSelect: function() { openInterfaceEdit(ifaceType, ifaceName); } });
    }
    items.push({ label: 'Rename', icon: ICON_PENCIL, onSelect: function() { openRenameInterfaceDialog(ifaceName); } });
    items.push({ label: 'Remove', icon: ICON_TRASH, danger: true, onSelect: function() { confirmRemoveInterface(ifaceType, ifaceName); } });
    return items;
}

function setInterfacePaused(ifaceType, ifaceName, paused) {
    var command = paused ? 'pause_interface' : 'resume_interface';
    RS.invoke(command, {
        args: {
            name: ifaceName,
            iface_type: ifaceType
        }
    }).then(function() {
        showToast(paused ? 'Pausing interface...' : 'Resuming interface...', 'toast-blue', 2500);
        refreshConfigInterfaces();
    }).catch(function(err) {
        showToast((err && err.message) || 'Failed to update interface', 'toast-red', 8000);
    });
}

function openInterfaceEdit(ifaceType, ifaceName) {
    var record = (_cachedConfigByName && _cachedConfigByName[ifaceName]) || null;
    var iface = record ? record.iface : null;
    if (!iface) {
        showToast('Interface settings are still loading', 'toast-yellow', 2500);
        refreshConfigInterfaces();
        return;
    }
    if (typeof openInterfaceEditModal === 'function') {
        openInterfaceEditModal(ifaceType, ifaceName, iface);
    }
}

// Runs on every stats_update — rates need a continuous series even off-tab.
function updateInterfaceHistory(stats) {
    var ifaces = interfaceStatsWithoutAutoPeerDoubleCount((stats && stats.interfaces) ? stats.interfaces : []);
    if (ifaces.length === 0) return;
    var currentIfaceNames = {};
    ifaces.forEach(function(iface) {
        var name = iface.name || 'unknown';
        currentIfaceNames[name] = true;
        if (!interfaceHistory[name]) interfaceHistory[name] = [];
        interfaceHistory[name].push({
            t: Date.now() / 1000,
            txb: iface.txb || 0,
            rxb: iface.rxb || 0,
        });
        if (interfaceHistory[name].length > 60) {
            interfaceHistory[name] = interfaceHistory[name].slice(-60);
        }
    });
    Object.keys(interfaceHistory).forEach(function(key) {
        if (!currentIfaceNames[key]) delete interfaceHistory[key];
    });
}

function renderInterfaceCards(stats) {
    var container = document.getElementById('interfaces-container');
    if (!container) return;
    var settingsView = document.getElementById('view-settings');
    if (settingsView && !settingsView.classList.contains('active')) return;

    var ifaces = interfaceStatsWithoutAutoPeerDoubleCount((stats && stats.interfaces) ? stats.interfaces : []);
    if (ifaces.length === 0) {
        var msg = (lastStats && lastStats.connected === false)
            ? '<div class="inline-warning">Connecting to hub node...</div>'
            : '<div class="inline-hint">No interfaces detected.</div>';
        container.innerHTML = msg;
        return;
    }

    container.innerHTML = ifaces.map(function(iface) {
        var name = iface.name || 'unknown';
        var status = iface.online !== undefined ? (iface.online ? 'up' : 'down') : 'up';
        var statusLabel = status.toUpperCase();
        var typeName = iface.type || '';
        var role = iface.role || 'normal';
        var displayType = interfaceTypeLabel(typeName, role);

        var hist = interfaceHistory[name] || [];
        var txRate = 0, rxRate = 0;
        if (hist.length >= 2) {
            var last = hist[hist.length - 1];
            var prev = hist[hist.length - 2];
            var dt = last.t - prev.t;
            if (dt > 0) {
                txRate = ((last.txb - prev.txb) * 8) / dt;
                rxRate = ((last.rxb - prev.rxb) * 8) / dt;
            }
        }

        var isLora = typeName.indexOf('RNode') >= 0;
        var isBle = isLora && typeof iface.port === 'string' && iface.port.indexOf('ble://') === 0;
        var isAuto = typeName.indexOf('AutoInterface') >= 0;
        var isTcp = typeName.indexOf('TCPClient') >= 0 || typeName.indexOf('TCPServer') >= 0;
        var extraClass = isBle ? ' interface-card-ble' : (isLora ? ' interface-card-lora' : (isAuto ? ' interface-card-auto' : (isTcp ? ' interface-card-tcp' : '')));
        var cardClass = 'interface-card' + extraClass;

        var flowIndicator = '';
        if (isTcp && (txRate > 0 || rxRate > 0)) {
            flowIndicator = '<span class="interface-flow-active" title="Data flowing">&#9679;</span>';
        }

        var loraStatsHtml = '';
        if (isLora) {
            var loraParts = [];
            if (iface.frequency) {
                loraParts.push('<span class="lora-stat">' + (iface.frequency / 1000000).toFixed(2) + ' MHz</span>');
            }
            if (iface.bandwidth) {
                loraParts.push('<span class="lora-stat">BW ' + (iface.bandwidth / 1000).toFixed(0) + 'k</span>');
            }
            if (iface.spreading_factor || iface.spreadingfactor) {
                loraParts.push('<span class="lora-stat">SF' + (iface.spreading_factor || iface.spreadingfactor) + '</span>');
            }
            if (iface.coding_rate || iface.codingrate) {
                loraParts.push('<span class="lora-stat">CR' + (iface.coding_rate || iface.codingrate) + '</span>');
            }
            if (iface.airtime_short !== undefined) {
                var airtimePct = (iface.airtime_short * 100).toFixed(1);
                var airtimeWarn = airtimePct > 50;
                var airtimeClass = airtimeWarn ? ' lora-stat-warn' : '';
                loraParts.push('<span class="lora-stat' + airtimeClass + '">Airtime ' + airtimePct + '%' + (airtimeWarn ? ' (high)' : '') + '</span>');
            }
            if (iface.airtime_long !== undefined) {
                var airtimeLong = (iface.airtime_long * 100).toFixed(1);
                loraParts.push('<span class="lora-stat" title="60-minute rolling airtime">1h: ' + airtimeLong + '%</span>');
            }
            if (iface.channel_load_short !== undefined) {
                var loadPct = (iface.channel_load_short * 100).toFixed(1);
                var loadWarn = loadPct > 50;
                var loadClass = loadWarn ? ' lora-stat-warn' : '';
                loraParts.push('<span class="lora-stat' + loadClass + '">Ch.Load ' + loadPct + '%' + (loadWarn ? ' (high)' : '') + '</span>');
            }
            if (iface.channel_load_long !== undefined) {
                var channelLoadLong = (iface.channel_load_long * 100).toFixed(1);
                loraParts.push('<span class="lora-stat" title="60-minute rolling channel load">1h: ' + channelLoadLong + '%</span>');
            }
            if (iface.noise_floor !== undefined && iface.noise_floor !== null) {
                var noiseClass = '';
                var noiseText = '';
                if (iface.noise_floor > -60) {
                    noiseClass = ' lora-stat-warn';
                    noiseText = ' (high)';
                } else if (iface.noise_floor <= -90) {
                    noiseClass = ' lora-stat-good';
                    noiseText = ' (good)';
                }
                loraParts.push('<span class="lora-stat' + noiseClass + '">Noise ' + iface.noise_floor + ' dBm' + noiseText + '</span>');
            }
            if (iface.bitrate !== undefined && iface.bitrate !== null) {
                var brStr = iface.bitrate >= 1000 ? (iface.bitrate / 1000).toFixed(1) + ' Kbps' : iface.bitrate + ' bps';
                loraParts.push('<span class="lora-stat" title="Effective link capacity">' + brStr + '</span>');
            }
            if (iface.battery_state !== undefined && iface.battery_state) {
                var batteryPct = iface.battery_percent !== undefined ? iface.battery_percent + '%' : '';
                var batteryClass = '';
                if (iface.battery_percent !== undefined && iface.battery_percent < 20) batteryClass = ' lora-stat-warn';
                loraParts.push('<span class="lora-stat' + batteryClass + '">Bat ' + iface.battery_state + (batteryPct ? ' ' + batteryPct : '') + '</span>');
            }
            if (iface.cpu_temp !== undefined && iface.cpu_temp !== null) {
                var tempClass = iface.cpu_temp > 75 ? ' lora-stat-warn' : '';
                loraParts.push('<span class="lora-stat' + tempClass + '" title="RNode CPU temperature">' + iface.cpu_temp.toFixed(0) + '°C</span>');
            }
            if (loraParts.length > 0) {
                var freqMhz = iface.frequency ? (iface.frequency / 1000000) : null;
                var guidanceText = 'Airtime and channel load below 50% are normal.';
                if (freqMhz && freqMhz >= 900 && freqMhz <= 930) {
                    guidanceText = 'Noise: -67 to -72 dBm typical for 915 MHz. ' + guidanceText;
                } else if (freqMhz && freqMhz >= 860 && freqMhz <= 870) {
                    guidanceText = 'Noise: -72 to -80 dBm typical for 868 MHz. ' + guidanceText;
                } else {
                    guidanceText = 'Lower noise floor (more negative) is better. ' + guidanceText;
                }
                loraStatsHtml = '<div class="interface-lora-stats">' + loraParts.join('') + '</div>' +
                    '<div class="lora-guidance">' + guidanceText + '</div>';
            }
        }

        return '<div class="' + cardClass + '">' +
            '<div class="interface-card-header">' +
                '<span class="interface-card-name" title="' + escapeHtml(name) + '">' + escapeHtml(friendlyInterfaceName(name, typeName, role)) + '</span>' +
                '<span class="interface-card-status ' + status + '">' + statusLabel + '</span>' +
                flowIndicator +
            '</div>' +
            '<div class="interface-card-stats">' +
                '<span class="interface-stat-label">TX</span>' +
                '<span class="interface-stat-val">' + prettySize(iface.txb || 0) + '</span>' +
                '<span class="interface-stat-label">RX</span>' +
                '<span class="interface-stat-val">' + prettySize(iface.rxb || 0) + '</span>' +
                '<span class="interface-stat-label">TX Rate</span>' +
                '<span class="interface-stat-val">' + prettySpeed(txRate) + '</span>' +
                '<span class="interface-stat-label">RX Rate</span>' +
                '<span class="interface-stat-val">' + prettySpeed(rxRate) + '</span>' +
            '</div>' +
            loraStatsHtml +
            '<div class="interface-card-type">' + escapeHtml(displayType) + '</div>' +
        '</div>';
    }).join('');
}

function renderPathTable(pathTable, stats) {
    var tbody = document.getElementById('path-table-body');
    var countEl = document.getElementById('path-table-count');
    if (!tbody || !pathTable) return;

    var summary = typeof pathCountSummary === 'function'
        ? pathCountSummary(stats || { path_table: pathTable })
        : { visible: pathTable.length, total: pathTable.length, truncated: false, label: String(pathTable.length) };
    countEl.textContent = summary.label + ' paths';
    countEl.title = summary.truncated ? 'Visible table rows of total known paths' : 'Known paths';

    if (pathTable.length === 0) {
        tbody.innerHTML = '<tr><td colspan="4" class="text-muted-color">No known paths.</td></tr>';
        return;
    }

    pathTable.sort(function(a, b) {
        return (b.timestamp || 0) - (a.timestamp || 0);
    });

    tbody.innerHTML = pathTable.map(function(p) {
        var destHash = p.hash || p.destination_hash || '';
        var viaHash = p.via || p.next_hop || '';
        var hops = p.hops !== undefined ? p.hops : '?';
        var age = '';
        if (p.timestamp || p.expires) {
            var elapsed = Date.now() / 1000 - (p.timestamp || 0);
            age = prettyTime(elapsed);
        }
        var ageClass = '';
        if (p.timestamp) {
            var secs = Date.now() / 1000 - p.timestamp;
            if (secs > 600) ageClass = ' class="text-status-error"';
            else if (secs > 120) ageClass = ' class="text-status-warning"';
            else ageClass = ' class="text-status-online"';
        }

        return '<tr>' +
            '<td>' + copyableHash(destHash) + '</td>' +
            '<td>' + (viaHash ? copyableHash(viaHash) : 'direct') + '</td>' +
            '<td>' + hops + '</td>' +
            '<td' + ageClass + '>' + age + '</td>' +
        '</tr>';
    }).join('');
}

function renderAnnounceList() {
    var container = document.getElementById('announce-list');
    var countEl = document.getElementById('announce-count');
    if (!container) return;

    if (countEl) countEl.textContent = announceCache.length;

    var subCountEl = document.getElementById('announce-sub-count');
    if (subCountEl) subCountEl.textContent = announceCache.length;

    var netAnnouncesEl = document.getElementById('net-stat-announces');
    if (netAnnouncesEl) netAnnouncesEl.textContent = announceCache.length;

    var dashAnnounceCount = document.getElementById('stat-announces');
    if (dashAnnounceCount) dashAnnounceCount.textContent = announceCache.length;

    if (announceCache.length === 0) {
        container.innerHTML = '<div class="inline-hint">No announces received yet.</div>';
        return;
    }

    container.innerHTML = announceCache.slice(0, 50).map(function(a) {
        var aspect = a.app_data || '';
        var ts = a.timestamp ? formatTime(a.timestamp) : '';
        var node = a.node ? friendlyNode(a.node) : '';
        var hops = a.hops !== null && a.hops !== undefined ? a.hops + 'h' : '';

        return '<div class="announce-entry">' +
            '<span class="announce-hash">' + copyableHash(a.hash || '') + '</span>' +
            '<span class="announce-aspect">' + escapeHtml(aspect.substring(0, 40)) + '</span>' +
            (node ? '<span class="announce-node">' + escapeHtml(node) + '</span>' : '') +
            (hops ? '<span class="announce-hops">' + hops + '</span>' : '') +
            '<span class="announce-time">' + ts + '</span>' +
        '</div>';
    }).join('');
}

function renderNetworkOverview(data) {
    var txEl = document.getElementById('net-stat-tx');
    var rxEl = document.getElementById('net-stat-rx');

    var ifaces = (data.interface_stats && data.interface_stats.interfaces) ? data.interface_stats.interfaces : [];
    var totals = interfaceStatsTotals(ifaces);
    if (txEl) txEl.textContent = prettySize(totals.txb);
    if (rxEl) rxEl.textContent = prettySize(totals.rxb);
}

function renderNetworkPulse(data) {
    var pulseId = document.getElementById('pulse-identity');
    if (pulseId) {
        var active = null;
        if (typeof identityList !== 'undefined') {
            for (var i = 0; i < identityList.length; i++) {
                if (identityList[i].is_active) { active = identityList[i]; break; }
            }
        }
        if (active) {
            var nickname = escapeHtml(active.display_name || active.nickname || 'Unnamed');
            var lxmfHash = active.lxmf_hash || '';
            var identityHash = active.hash || '';
            var avatarHtml = (typeof identityAvatar === 'function') ? identityAvatar(lxmfHash || identityHash, 32) : '';
            pulseId.innerHTML =
                '<div style="flex-shrink:0;">' + avatarHtml + '</div>' +
                '<div style="min-width:0;">' +
                    '<div class="pulse-identity-name">' + nickname + '</div>' +
                    '<div class="pulse-identity-hash">' + lxmfHash + '</div>' +
                '</div>';
        }
    }

    renderNetworkOverview(data);
}

var _connSectionMap = {
    lora: { types: ['RNodeInterface', 'RNodeMultiInterface'], isSerial: true },
    ble: { types: ['RNodeInterface', 'RNodeMultiInterface'], isBle: true },
    tcp: { types: ['TCPClientInterface'] },
    host: { types: ['TCPServerInterface'] },
    local: { types: ['AutoInterface'] }
};

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
    // RNode is always LoRa, regardless of transport (BLE, USB serial).
    // The Bluetooth section is reserved for Bluetooth Peer peering only.
    if (typeName.indexOf('RNode') >= 0) return 'lora';
    if (typeName.indexOf('BlePeer') >= 0) return 'ble';
    if (typeName.indexOf('TCPClient') >= 0) return 'tcp';
    if (typeName.indexOf('TCPServer') >= 0) return 'host';
    if (typeName.indexOf('AutoInterface') >= 0) return 'local';
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

// Grace window: render 'Identifying peer\u2026' before the first signed
// announce arrives, then fall back to the BLE address.
var BLE_PEER_IDENTIFYING_GRACE_MS = 5000;

// Identity-aware label: contact name > truncated hash > grace placeholder >
// raw BLE address. Returns { label, title } for tooltip preservation.
function _resolveBlePeerLabel(peer) {
    var addr = peer.address || '';
    var idHash = peer.identity_hash || '';
    if (idHash) {
        if (typeof PeersCache !== 'undefined' && PeersCache && typeof PeersCache.get === 'function') {
            var entry = PeersCache.get(idHash);
            if (entry && entry.display_name && entry.display_name !== '' && entry.display_name !== idHash) {
                var name = entry.display_name;
                if (name.length > 40) name = name.substring(0, 40) + '\u2026';
                return { label: name, title: idHash };
            }
        }
        return { label: typeof shortHash === 'function' ? shortHash(idHash, 8, 4) : idHash.substring(0, 12) + '\u2026', title: idHash };
    }
    // Defer raw BLE address until grace window elapses to avoid a 1-2s UUID flash.
    var connectedAt = peer.connected_at || 0;
    if (connectedAt && Date.now() - connectedAt < BLE_PEER_IDENTIFYING_GRACE_MS) {
        return { label: 'Identifying peer\u2026', title: addr || 'Identifying peer' };
    }
    return { label: addr, title: addr };
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
        .filter(function(p) { return p && p.connected === true; });
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

function renderBlePeerRow(peer) {
    var addr = peer.address || '';
    var addresses = Array.isArray(peer.addresses) && peer.addresses.length ? peer.addresses : [addr];
    var addressList = addresses.filter(Boolean).join(',');
    var resolved = _resolveBlePeerLabel(peer);
    var protocol = peer.protocol || 'Ratspeak';
    var protoClass = protocol === 'Columba' ? 'badge-columba' : 'badge-ratspeak';
    // connected_at is epoch ms; RS.relativeTime takes seconds.
    var ago = RS.relativeTime(Math.floor(peer.connected_at / 1000));

    return '<div class="conn-iface-row ble-peer-row" data-peer-address="' + escapeHtml(addr) + '" data-peer-addresses="' + escapeHtml(addressList) + '" data-peer-protocol="' + escapeHtml(protocol) + '" role="button" tabindex="0">' +
        '<span class="conn-iface-dot up" title="Connected"></span>' +
        '<span class="conn-iface-name ble-peer-id" title="' + escapeHtml(resolved.title) + '">' + escapeHtml(resolved.label) + '</span>' +
        '<span class="ble-peer-protocol ' + protoClass + '">' + escapeHtml(protocol) + '</span>' +
        '<span class="ble-peer-time">' + escapeHtml(ago) + '</span>' +
        '<button class="ble-peer-kebab" aria-label="Peer actions" data-peer-address="' + escapeHtml(addr) + '" data-peer-addresses="' + escapeHtml(addressList) + '">\u22ee</button>' +
    '</div>';
}

function _renderBleSection(bodyEl, sectionEl, countEl) {
    var peers = _bleVisiblePeersFromCache();
    var count = peers.length;
    if (countEl) {
        countEl.textContent = count;
        countEl.className = 'conn-section-count' + (count > 0 ? ' has-active' : '');
    }
    if (sectionEl) {
        if (count > 0) sectionEl.classList.remove('inactive');
        else sectionEl.classList.add('inactive');
        var collapsed = (typeof shouldSectionBeCollapsed === 'function')
            ? shouldSectionBeCollapsed('ble', count > 0) : (count === 0);
        sectionEl.classList.toggle('collapsed', collapsed);
        var headerEl = sectionEl.querySelector('.conn-section-header');
        if (headerEl) headerEl.setAttribute('aria-expanded', collapsed ? 'false' : 'true');
    }
    if (count === 0) {
        var msg;
        if (window._blePeerEnabled && window._blePeerPeripheralUnavailable) {
            msg = 'Central-only \u2014 scanning for peers\u2026';
        } else if (window._blePeerEnabled) {
            msg = 'Scanning for peers\u2026';
        } else {
            msg = 'No active peers';
        }
        bodyEl.innerHTML = '<div class="conn-iface-empty">' + msg + '</div>';
        return;
    }
    bodyEl.innerHTML = peers.map(renderBlePeerRow).join('');
}

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

function clearNetworkInterfaceCaches(opts) {
    opts = opts || {};
    var empty = emptyInterfaceConfigPayload();
    window._hubInterfacesData = empty;
    window._hubInterfacesCached = false;
    window._autoEnabled = false;
    window._autoUnavailable = null;
    _cachedConfigIfaces = empty;
    _cachedConfigByName = {};
    interfaceHistory = {};
    lastStats = null;
    _anyInterfaceOnline = null;
    _connectionsHasRendered = false;
    _connectionsRenderScheduled = false;
    if (_connectionsThrottleTimer) {
        clearTimeout(_connectionsThrottleTimer);
        _connectionsThrottleTimer = null;
    }
    if (_connectionsFirstLoadTimer) {
        clearTimeout(_connectionsFirstLoadTimer);
        _connectionsFirstLoadTimer = null;
    }
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

function _renderConnectionsFromCache() {
    var ifaces = _cachedConfigIfaces;
    var configByName = _cachedConfigByName;
    if (!ifaces) return;

        // Live transport stats are primary; config data only enriches.
        var allIfaces = [];
        var matchedConfigNames = {};
        if (lastStats && lastStats.interface_stats && lastStats.interface_stats.interfaces) {
            interfaceStatsWithoutAutoPeerDoubleCount(lastStats.interface_stats.interfaces).forEach(function(li) {
                if (!isUserFacingInterface(li)) return;
                var configMatch = null;
                Object.keys(configByName).forEach(function(cn) {
                    if (_statsNameMatchesConfig(li.name, cn)) configMatch = configByName[cn];
                });
                if (configMatch && configMatch.iface && configMatch.iface.name) {
                    matchedConfigNames[configMatch.iface.name] = true;
                }
                var section = classifyInterface(li);
                // Live stats lack 'type'; fall back to config match for dyn ifaces.
                if (!section && configMatch) {
                    section = interfaceSectionForConfigType(configMatch.ifaceType) || configMatch.ifaceType;
                }
                if (!section) return;
                var ifaceType = configMatch ? configMatch.ifaceType : section;
                var paused = !!(configMatch && configMatch.iface && !isInterfaceConfigEnabled(configMatch.iface));
                var renderIface = configMatch && configMatch.iface
                    ? Object.assign({}, configMatch.iface, { role: li.role || configMatch.iface.role, type: li.type || configMatch.iface.type })
                    : li;
                allIfaces.push({ iface: renderIface, section: section, ifaceType: ifaceType, paused: paused });
            });
        }
        Object.keys(configByName).forEach(function(cn) {
            if (matchedConfigNames[cn]) return;
            var record = configByName[cn];
            if (!record || !record.iface || isInterfaceConfigEnabled(record.iface)) return;
            var section = interfaceSectionForConfigType(record.ifaceType);
            if (!section) return;
            allIfaces.push({
                iface: record.iface,
                section: section,
                ifaceType: record.ifaceType,
                paused: true
            });
        });

        var grouped = { lora: [], tcp: [], host: [], local: [], ble: [] };
        allIfaces.forEach(function(a) {
            if (grouped[a.section]) grouped[a.section].push(a);
        });

        var sections = ['lora', 'tcp', 'host', 'local', 'ble'];
        sections.forEach(function(sectionKey) {
            var bodyEl = document.getElementById('conn-body-' + sectionKey);
            var countEl = document.getElementById('conn-count-' + sectionKey);
            var sectionEl = bodyEl ? bodyEl.closest('.conn-section') : null;
            if (!bodyEl) return;

            // BLE section drives from per-peer cache, not interfaces.
            if (sectionKey === 'ble') {
                _renderBleSection(bodyEl, sectionEl, countEl);
                return;
            }

            var items = grouped[sectionKey] || [];
            var count = items.length;

            if (countEl) {
                countEl.textContent = count;
                countEl.className = 'conn-section-count' + (count > 0 ? ' has-active' : '');
            }

            // Default: empty -> collapsed; populated -> open.
            if (sectionEl) {
                if (count > 0) sectionEl.classList.remove('inactive');
                else sectionEl.classList.add('inactive');

                var collapsed = shouldSectionBeCollapsed(sectionKey, count > 0);
                sectionEl.classList.toggle('collapsed', collapsed);

                var headerEl = sectionEl.querySelector('.conn-section-header');
                if (headerEl) headerEl.setAttribute('aria-expanded', collapsed ? 'false' : 'true');
            }

            if (count === 0) {
                bodyEl.innerHTML = '<div class="conn-iface-empty">No active interfaces</div>';
                return;
            }

            var html = '';
            items.forEach(function(item) {
                var iface = item.iface;
                var ifaceType = item.ifaceType;
                var paused = !!item.paused;
                var name = iface.name || 'unknown';
                var typeName = iface.type || '';

                var liveData = (typeof getInterfaceLiveStatus === 'function') ? getInterfaceLiveStatus(name) : null;
                var online = liveData ? (liveData.online !== false) : false;
                var statusClass = paused ? 'paused' : (online ? 'up' : 'down');

                var txb = 0, rxb = 0, txRate = 0, rxRate = 0;
                if (liveData) {
                    txb = liveData.txb || 0;
                    rxb = liveData.rxb || 0;
                }
                var hist = interfaceHistory[name] || [];
                if (hist.length >= 2) {
                    var last = hist[hist.length - 1];
                    var prev = hist[hist.length - 2];
                    var dt = last.t - prev.t;
                    if (dt > 0) {
                        txRate = ((last.txb - prev.txb) * 8) / dt;
                        rxRate = ((last.rxb - prev.rxb) * 8) / dt;
                        if (!txb) txb = last.txb;
                        if (!rxb) rxb = last.rxb;
                    }
                }

                var isActive = txRate > 0 || rxRate > 0;
                var dotClass = statusClass + (!paused && isActive ? ' active' : '');

                var detail = (typeof getIfaceDetailText === 'function') ? getIfaceDetailText(iface, ifaceType) : '';

                var displayName = (typeof friendlyInterfaceName === 'function') ? friendlyInterfaceName(name, typeName, iface.role || 'normal') : name;

                // Multicast join rejected (iOS entitlement / Linux NIC vanish);
                // pill surfaces this so the empty peer list isn't mistaken for a bug.
                var pillHtml = '';
                if (sectionKey === 'local' && window._autoUnavailable &&
                    window._autoUnavailable.interface === name) {
                    var iosPill = (typeof isIOS === 'function') && isIOS();
                    if (iosPill) {
                        pillHtml = '<span class="conn-iface-pill" ' +
                            'title="Apple multicast entitlement is required for local Wi-Fi peer discovery. We have requested it; coverage will appear automatically once approved." ' +
                            '>' +
                            'Pending Apple approval' +
                            '</span>';
                    } else {
                        pillHtml = '<span class="conn-iface-pill" ' +
                            'title="' + escapeHtml(window._autoUnavailable.reason || 'Multicast unavailable') + '" ' +
                            '>' +
                            'Multicast unavailable' +
                            '</span>';
                    }
                }
                if (paused) {
                    pillHtml += '<span class="conn-iface-pill conn-iface-pill-paused">Paused</span>';
                }

                // Augment label with non-default group ID (matches Python rnsd).
                var groupSuffix = '';
                if (sectionKey === 'local' && iface.group_id && iface.group_id !== 'reticulum') {
                    groupSuffix = ' <span class="conn-iface-group">(' + escapeHtml(iface.group_id) + ')</span>';
                }

                html += '<div class="conn-iface-row' + (paused ? ' is-paused' : '') + '" data-iface-name="' + escapeHtml(name) + '" data-iface-type="' + escapeHtml(ifaceType) + '" role="button" tabindex="0">' +
                    '<span class="conn-iface-dot ' + dotClass + '"></span>' +
                    '<span class="conn-iface-main">' +
                        '<span class="conn-iface-titleline">' +
                            '<span class="conn-iface-name" title="' + escapeHtml(name) + '">' + escapeHtml(displayName) + groupSuffix + '</span>' +
                            pillHtml +
                        '</span>' +
                        '<span class="conn-iface-stats">' +
                            '<span title="TX">\u2191 ' + prettySize(txb) + '</span>' +
                            '<span title="RX">\u2193 ' + prettySize(rxb) + '</span>' +
                        '</span>' +
                    '</span>' +
                '</div>';
            });

            bodyEl.innerHTML = html;

            if (sectionEl) {
                var hasTraffic = items.some(function(item) {
                    var h = interfaceHistory[item.iface.name] || [];
                    if (h.length < 2) return false;
                    var l = h[h.length - 1], p = h[h.length - 2];
                    var d = l.t - p.t;
                    return d > 0 && ((l.txb - p.txb) > 0 || (l.rxb - p.rxb) > 0);
                });
                sectionEl.classList.toggle('active-traffic', hasTraffic);
            }
        });

    var settingsContainer = document.getElementById('settings-interfaces-container');
    if (settingsContainer) {
        _renderSettingsInterfacesFromData(ifaces, settingsContainer);
    }
}

function _renderSettingsInterfacesFromData(ifaces, container) {
    var hasAny = (ifaces.rnode && ifaces.rnode.length) ||
                 (ifaces.auto && ifaces.auto.length) ||
                 (ifaces.tcp_client && ifaces.tcp_client.length) ||
                 (ifaces.tcp_server && ifaces.tcp_server.length) ||
                 (ifaces.backbone_client && ifaces.backbone_client.length) ||
                 (ifaces.backbone_server && ifaces.backbone_server.length);

    var headerEl = document.getElementById('conn-active-header');
    var countEl = document.getElementById('conn-active-count');
    var total = (ifaces.rnode||[]).length + (ifaces.auto||[]).length +
                (ifaces.tcp_client||[]).length + (ifaces.tcp_server||[]).length +
                (ifaces.backbone_client||[]).length + (ifaces.backbone_server||[]).length;

    if (!hasAny) {
        container.innerHTML = '';
        if (headerEl) headerEl.style.display = 'none';
        return;
    }

    if (headerEl) headerEl.style.display = '';
    if (countEl) countEl.textContent = total;

    container.innerHTML = '';
    var allRnodes = ifaces.rnode || [];
    var bleIfaces = allRnodes.filter(function(i) { return (i.port || '').indexOf('ble://') === 0; });
    var serialIfaces = allRnodes.filter(function(i) { return (i.port || '').indexOf('ble://') !== 0; });
    if (typeof renderSettingsIfaceSection === 'function') {
        renderSettingsIfaceSection(container, 'LoRa Radios', serialIfaces, 'rnode');
        renderSettingsIfaceSection(container, 'BLE Radios', bleIfaces, 'rnode');
        renderSettingsIfaceSection(container, 'Local Network', ifaces.auto || [], 'auto');
        renderSettingsIfaceSection(container, 'TCP Connections', ifaces.tcp_client || [], 'tcp_client');
        renderSettingsIfaceSection(container, 'TCP Servers', ifaces.tcp_server || [], 'tcp_server');
        renderSettingsIfaceSection(container, 'Backbone Connections', ifaces.backbone_client || [], 'backbone_client');
        renderSettingsIfaceSection(container, 'Backbone Servers', ifaces.backbone_server || [], 'backbone_server');
    }
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

function showInterfaceActionSheet(ifaceType, ifaceName) {
    var overlay = document.getElementById('iface-action-overlay');
    var sheet = document.getElementById('iface-action-sheet');
    var titleEl = document.getElementById('iface-action-title');
    var itemsEl = document.getElementById('iface-action-items');
    if (!overlay || !sheet || !titleEl || !itemsEl) return;

    var displayName = (typeof friendlyInterfaceName === 'function')
        ? friendlyInterfaceName(ifaceName, '')
        : ifaceName;
    if (typeof setBottomSheetTitleWithIcon === 'function' && typeof interfaceSheetIconTypeForInterface === 'function') {
        setBottomSheetTitleWithIcon(titleEl, displayName, interfaceSheetIconTypeForInterface(ifaceType));
    } else {
        titleEl.textContent = displayName;
    }

    var items = buildIfaceActionItems(ifaceType, ifaceName);
    itemsEl.innerHTML = '';
    items.forEach(function(item, idx) {
        if (item.separator) {
            var divider = document.createElement('div');
            divider.className = 'bottom-sheet-divider';
            itemsEl.appendChild(divider);
            return;
        }
        var btn = document.createElement('button');
        btn.className = 'bottom-sheet-item' + (item.danger ? ' bottom-sheet-danger' : '');
        if (item.disabled) {
            btn.disabled = true;
            btn.classList.add('disabled');
        }
        btn.innerHTML = item.icon + '<span>' + item.label + '</span>';
        if (!item.disabled) {
            btn.onclick = function() {
                closeInterfaceActionSheet();
                setTimeout(function() {
                    if (typeof item.onSelect === 'function') item.onSelect();
                }, 150);
            };
        }
        itemsEl.appendChild(btn);
    });

    overlay.classList.add('active');
    sheet.classList.add('open');
    overlay.onclick = function() { closeInterfaceActionSheet(); };
}

function closeInterfaceActionSheet() {
    var overlay = document.getElementById('iface-action-overlay');
    var sheet = document.getElementById('iface-action-sheet');
    if (overlay) overlay.classList.remove('active');
    if (sheet) sheet.classList.remove('open');
}

function toggleConnSection(headerEl) {
    var section = headerEl.closest('.conn-section');
    if (!section) return;
    section.classList.toggle('collapsed');
    var collapsed = section.classList.contains('collapsed');
    headerEl.setAttribute('aria-expanded', collapsed ? 'false' : 'true');
    var key = section.dataset.ifaceType;
    if (key) setConnSectionPref(key, collapsed ? 'closed' : 'open');
}

// Header has a nested + button so can't be a real <button>;
// role="button" + tabindex + this keydown handler is the WCAG alternative.
function handleConnSectionKey(e) {
    if (e.key === 'Enter' || e.key === ' ' || e.key === 'Spacebar') {
        e.preventDefault();
        toggleConnSection(e.currentTarget);
    }
}

function renderTrafficTable(rateTable) {
    var container = document.getElementById('traffic-container');
    var countEl = document.getElementById('traffic-count');
    if (!container) return;

    if (!rateTable || rateTable.length === 0) {
        container.innerHTML = '<div class="inline-hint">No rate limit entries.</div>';
        if (countEl) countEl.textContent = '0 entries';
        return;
    }

    if (countEl) countEl.textContent = rateTable.length + ' entr' + (rateTable.length !== 1 ? 'ies' : 'y');

    var html = '<div class="traffic-table-wrapper"><table class="traffic-table">' +
        '<thead><tr><th>Destination</th><th>Status</th><th>Last Seen</th></tr></thead><tbody>';

    for (var i = 0; i < rateTable.length; i++) {
        var entry = rateTable[i];
        var destHash = entry.hash || entry.destination_hash || '';
        var rate = entry.rate !== undefined ? entry.rate : (entry.traffic_rate !== undefined ? entry.traffic_rate : 0);
        var isActive = rate > 0;
        var badgeClass = isActive ? 'active' : 'idle';
        var rateText = isActive ? prettySpeed(rate * 8) : 'Idle';
        var lastActivity = '';
        if (entry.last !== undefined) {
            lastActivity = prettyTime(Date.now() / 1000 - entry.last);
        } else if (entry.last_activity !== undefined) {
            lastActivity = prettyTime(Date.now() / 1000 - entry.last_activity);
        }

        html += '<tr>' +
            '<td>' + copyableHash(destHash) + '</td>' +
            '<td><span class="traffic-rate-badge ' + badgeClass + '">' + rateText + '</span></td>' +
            '<td>' + (lastActivity || '&mdash;') + '</td>' +
        '</tr>';
    }

    html += '</tbody></table></div>';
    container.innerHTML = html;
}

function renderAlert(alert) {
    var colorClass = alert.level === 'critical' ? 'toast-red' : 'toast-orange';
    var duration = 5000;
    showToast(alert.message, colorClass, duration);
}

var _announceRenderScheduled = false;
function _scheduleAnnounceRender() {
    if (_announceRenderScheduled) return;
    _announceRenderScheduled = true;
    requestAnimationFrame(function() {
        _announceRenderScheduled = false;
        renderAnnounceList();
    });
}
RS.listen('announce_received', function(data) {
    announceCache.unshift(data);
    if (announceCache.length > 200) announceCache.length = 200;
    // rAF coalesce — a busy hub floods hundreds of announces per second.
    _scheduleAnnounceRender();
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
    updateInterfaceHistory(data.interface_stats || data);
    renderPathTable(data.path_table || [], data);
    renderNetworkOverview(data);
    renderTrafficTable(data.rate_table || []);
    // Re-render from cached config so online status + bytes stay in sync.
    if (isViewActive('network') && typeof _renderConnectionsFromCache === 'function') {
        _renderConnectionsFromCache();
    }
};

['stat-paths', 'stat-announces'].forEach(function(id) {
    var el = document.getElementById(id);
    if (el && el.parentElement) {
        el.parentElement.style.cursor = 'pointer';
        el.parentElement.addEventListener('click', function() {
            if (typeof switchView === 'function') switchView('network');
        });
    }
});

var networkAnnounceBtn = document.getElementById('network-announce-btn');
if (networkAnnounceBtn) {
    var networkAnnounceLabel = networkAnnounceBtn.querySelector('span') || networkAnnounceBtn;
    networkAnnounceBtn.addEventListener('click', function() {
        if (!tryTriggerAnnounce()) return;
        networkAnnounceBtn.disabled = true;
        if (networkAnnounceBtn.dataset) networkAnnounceBtn.dataset.announcePending = '1';
        networkAnnounceLabel.textContent = 'Announcing...';
        setTimeout(function() {
            if (networkAnnounceBtn.dataset && networkAnnounceBtn.dataset.announcePending !== '1') return;
            if (networkAnnounceBtn.dataset) delete networkAnnounceBtn.dataset.announcePending;
            networkAnnounceBtn.disabled = false;
            networkAnnounceLabel.textContent = 'Announce';
        }, 10000);
    });
}

function initNetworkSubtabs() {
    var bar = document.getElementById('network-subtabs');
    var main = document.querySelector('.network-main');
    if (!bar || !main) return;
    main.classList.add('subtab-connections');
    bar.addEventListener('click', function(e) {
        var btn = e.target.closest('.network-subtab-btn');
        if (!btn) return;
        var tab = btn.dataset.subtab;
        bar.querySelectorAll('.network-subtab-btn').forEach(function(b) { b.classList.remove('active'); });
        btn.classList.add('active');
        main.className = main.className.replace(/subtab-\w+/g, '').trim() + ' subtab-' + tab;
    });
}
initNetworkSubtabs();

(function() {
    var container = document.querySelector('.network-connections');
    if (!container) return;
    container.addEventListener('click', function(e) {
        var row = e.target.closest('.conn-iface-row');
        if (!row) return;
        var ifaceType = row.dataset.ifaceType;
        var ifaceName = row.dataset.ifaceName;
        if (!ifaceType || !ifaceName) return;
        if (typeof haptic === 'function') haptic('selection');
        if (typeof isMobile === 'function' && isMobile()) {
            showInterfaceActionSheet(ifaceType, ifaceName);
        } else if (typeof actionPopover === 'function') {
            actionPopover(row, buildIfaceActionItems(ifaceType, ifaceName));
        } else {
            showInterfaceActionSheet(ifaceType, ifaceName);
        }
    });
    container.addEventListener('keydown', function(e) {
        if (e.key !== 'Enter' && e.key !== ' ') return;
        var row = e.target.closest('.conn-iface-row');
        if (!row) return;
        e.preventDefault();
        row.click();
    });
})();
