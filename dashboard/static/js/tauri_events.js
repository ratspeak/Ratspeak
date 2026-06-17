// Drives the post-connect summary toast (new paths / announces).
var connectionWatcher = null;

var _initialConnectDone = false;

function showConnectionSummary() {
    if (!connectionWatcher) return;
    connectionWatcher.shown = true;
    connectionWatcher = null;
}

// One-shot bootstrap; subsequent updates arrive via push events.
function _rsBootstrapOnLoad() {
    // Race-free check for a locked protected identity at boot (the
    // hardware_locked event can fire before this listener attaches).
    RS.invoke('api_startup_progress').then(function(data) {
        if (data && data.stage === 'hw_locked' && typeof showHwUnlock === 'function') {
            showHwUnlock(data.hw_locked, data.hw_locked_kind);
        }
    }).catch(function() {});
    if (window.RS && RS.audioPlayback && typeof RS.audioPlayback.ensure === 'function') {
        RS.audioPlayback.ensure({ installUnlock: true }).catch(function() {});
    }
    RS.invoke('api_announces').then(function(data) {
        if (Array.isArray(data)) {
            announceCache = data;
            if (typeof renderAnnounceList === 'function') renderAnnounceList();
        }
    }).catch(function() {});
    if (typeof loadIdentities === 'function') loadIdentities();
    if (typeof loadConversations === 'function') loadConversations();
    if (typeof updateBlockedCount === 'function') updateBlockedCount();

    // Hydrate from SQLite so the Peers tab paints without waiting on the poll loop.
    if (typeof PeersCache !== 'undefined' && PeersCache && typeof PeersCache.init === 'function') {
        PeersCache.init().catch(function() {});
    }

    var peersView = document.getElementById('view-peers');
    if (peersView && peersView.classList.contains('active') && typeof initPeersView === 'function') {
        initPeersView();
    }

    // Wire order matches UI: sidebar list first, then active conversation body.
    if (typeof lxmfActiveContact !== 'undefined' && lxmfActiveContact) {
        RS.invoke('get_conversation', { hash: lxmfActiveContact }).catch(function() {});
    }
}

if (document.readyState === 'complete' || document.readyState === 'interactive') {
    _rsBootstrapOnLoad();
} else {
    document.addEventListener('DOMContentLoaded', _rsBootstrapOnLoad);
}

// Native notification tap routing. When the app is backgrounded and the user
// taps a notification, deep-link to the originating conversation/game via the
// `route` extra the backend attaches (lxmf:<hash> / lrgp:<session_id>).
// Android-only in practice today: desktop notify-rust exposes no tap callback
// and iOS notifications are stubbed pre-release (see ratspeak-tauri notifier.rs).
function _routeNotificationTap(payload) {
    if (!payload || typeof payload !== 'object') return;
    // Android delivers {inputValue, actionId, notification:{...,extra}}; a flat
    // shape (extra at top level) is tolerated for other backends.
    var extra = (payload.notification && payload.notification.extra) || payload.extra;
    var route = extra && extra.route;
    if (typeof route !== 'string') return;
    var sep = route.indexOf(':');
    if (sep < 0) return;
    var kind = route.slice(0, sep);
    var id = route.slice(sep + 1);
    if (!id) return;
    if (kind === 'lxmf') {
        if (typeof openConversationWith === 'function') openConversationWith(id);
    } else if (kind === 'lrgp') {
        if (typeof window.openGameSession === 'function') window.openGameSession(id);
    }
    // TODO(call menu): route kind === 'lxst' once the dedicated call menu exists;
    // for now an unhandled kind just focuses the app, which is the desired behavior.
}

function _initNotificationTapRouting() {
    var core = window.__TAURI__ && window.__TAURI__.core;
    if (!core || typeof core.addPluginListener !== 'function') return false;
    var p = core.addPluginListener('notification', 'actionPerformed', _routeNotificationTap);
    if (p && typeof p.catch === 'function') {
        p.catch(function(e) { window.RS.diag('warn', '[notif-tap] register failed:', e); });
    }
    return true;
}

(function _armNotificationTapRouting() {
    if (_initNotificationTapRouting()) return;
    // Tauri globals can inject after DOMContentLoaded on iOS WKWebView; poll briefly.
    var attempts = 0;
    var iv = setInterval(function() {
        attempts++;
        if (_initNotificationTapRouting() || attempts >= 20) clearInterval(iv);
    }, 50);
})();

RS.listen('stats_update', function(data) {
    if (!data || typeof data !== 'object') return;
    lastStats = data;
    var ifaces = (data.interface_stats && Array.isArray(data.interface_stats.interfaces)) ? data.interface_stats.interfaces : null;
    if (ifaces) {
        _anyInterfaceOnline = ifaces.some(function(i) { return i && i.online === true; });
        if (_anyInterfaceOnline && typeof scheduleFirstRunTooltip === 'function') {
            scheduleFirstRunTooltip(600);
        }
    }
    renderStats(data);

    // path_age/hops/via change with path_table; renderer dirty-keys dedupe.
    if (typeof renderContactList === 'function') renderContactList();
    if (typeof renderStandaloneContactList === 'function') renderStandaloneContactList();
    if (typeof renderNetworkContactList === 'function') renderNetworkContactList();
    if (typeof updatePeersFromStats === 'function') updatePeersFromStats();

    var dashView = document.getElementById('view-dashboard');
    if (dashView && dashView.classList.contains('active') && typeof renderDashboardSummaries === 'function') {
        renderDashboardSummaries(data);
    }

    // Throttle to immediate-then-5s after first paint.
    if (typeof renderConnectionsTable === 'function') {
        var _peersAvailable = (typeof PeersCache !== 'undefined' && PeersCache && PeersCache.size() > 0);
        if (!_connectionsHasRendered) {
            if (_peersAvailable) {
                _connectionsHasRendered = true;
                if (_connectionsFirstLoadTimer) {
                    clearTimeout(_connectionsFirstLoadTimer);
                    _connectionsFirstLoadTimer = null;
                }
                renderConnectionsTable(PeersCache.enriched());
            } else if (data.connected && !_connectionsFirstLoadTimer) {
                // Connected but no peers — wait for poll loop, fall back at 5s.
                _connectionsFirstLoadTimer = setTimeout(function() {
                    if (!_connectionsHasRendered) {
                        _connectionsHasRendered = true;
                        var v = (typeof PeersCache !== 'undefined' && PeersCache) ? PeersCache.enriched() : [];
                        renderConnectionsTable(v);
                    }
                }, 5000);
            }
        } else if (!_connectionsRenderScheduled) {
            _connectionsRenderScheduled = true;
            _connectionsThrottleTimer = setTimeout(function() {
                _connectionsRenderScheduled = false;
                _connectionsThrottleTimer = null;
                var v = (typeof PeersCache !== 'undefined' && PeersCache) ? PeersCache.enriched() : [];
                renderConnectionsTable(v);
            }, 5000);
        }
    }

    var msgView = document.getElementById('view-message');
    if (msgView && msgView.classList.contains('active') && typeof updateMessageReachability === 'function') {
        updateMessageReachability();
    }

    if (document.getElementById('node-modal').classList.contains('open')) {
        if (typeof updateHubModalStatusDots === 'function') updateHubModalStatusDots();
    }

    var networkView = document.getElementById('view-network');
    if (networkView && networkView.classList.contains('active')) {
        if (typeof renderNetworkPulse === 'function') renderNetworkPulse(data);

        document.querySelectorAll('.conn-iface-row').forEach(function(row) {
            var ifaceName = row.dataset.ifaceName;
            if (!ifaceName) return;
            var dot = row.querySelector('.conn-iface-dot');
            if (!dot) return;
            var liveData = (typeof getInterfaceLiveStatus === 'function') ? getInterfaceLiveStatus(ifaceName) : null;
            var hist = interfaceHistory[ifaceName] || [];
            var isActive = false;
            if (hist.length >= 2) {
                var l = hist[hist.length - 1], p = hist[hist.length - 2];
                var d = l.t - p.t;
                isActive = d > 0 && ((l.txb - p.txb) > 0 || (l.rxb - p.rxb) > 0);
            }
            dot.className = 'conn-iface-dot';
            if (liveData) {
                dot.classList.add(liveData.online ? 'up' : 'down');
            } else {
                dot.classList.add('up');
            }
            if (isActive) dot.classList.add('active');
        });
    }
});

// Single batched event per poll keeps WebView JNI global refs flat.
// `peer_updated` (singular) kept as forward-compat shim.
RS.listen('peers_updated', function(payload) {
    if (typeof PeersCache === 'undefined' || !PeersCache) return;
    var rows = (payload && Array.isArray(payload.peers)) ? payload.peers : null;
    if (rows) PeersCache.applyBatch(rows);
});
RS.listen('peer_updated', function(payload) {
    if (typeof PeersCache !== 'undefined' && PeersCache) PeersCache.applyUpdated(payload);
});
RS.listen('peer_removed', function(payload) {
    if (typeof PeersCache !== 'undefined' && PeersCache) {
        var hash = (payload && typeof payload === 'object') ? payload.hash : payload;
        PeersCache.applyRemoved(hash);
    }
});

function _renderPathCacheCleared() {
    if (typeof lastStats !== 'undefined' && lastStats && typeof lastStats === 'object') {
        lastStats.path_table = [];
        lastStats.path_index = {};
        lastStats.path_table_total = 0;
        lastStats.path_table_truncated = false;
        if (typeof renderStats === 'function') renderStats(lastStats);
    }
    if (typeof updatePeersFromStats === 'function') updatePeersFromStats();
    if (typeof renderContactList === 'function') renderContactList();
    if (typeof renderStandaloneContactList === 'function') renderStandaloneContactList();
    if (typeof renderNetworkContactList === 'function') renderNetworkContactList();
}

function _reloadPeersAfterCacheClear() {
    if (typeof PeersCache === 'undefined' || !PeersCache) return;
    if (typeof RS === 'undefined' || typeof RS.invoke !== 'function') {
        if (typeof PeersCache.clear === 'function') PeersCache.clear();
        return;
    }
    RS.invoke('api_get_peers_snapshot').then(function(rows) {
        if (typeof PeersCache.replace === 'function') {
            PeersCache.replace(rows);
        } else {
            PeersCache.clear();
            if (typeof PeersCache.init === 'function') PeersCache.init().catch(function() {});
        }
    }).catch(function() {
        if (typeof PeersCache.clear === 'function') PeersCache.clear();
    });
}

RS.listen('paths_cleared', function() {
    _renderPathCacheCleared();
});

RS.listen('announces_cleared', function() {
    if (typeof announceCache !== 'undefined') announceCache = [];
    if (typeof renderAnnounceList === 'function') renderAnnounceList();
    _reloadPeersAfterCacheClear();
});

var _eventRenderScheduled = false;
function _scheduleEventRender() {
    if (_eventRenderScheduled) return;
    _eventRenderScheduled = true;
    requestAnimationFrame(function() {
        _eventRenderScheduled = false;
        renderLog();
        if (typeof renderCockpitEvents === 'function') renderCockpitEvents();
    });
}
RS.listen('event', function(ev) {
    if (!ev || typeof ev !== 'object') return;
    // Throttle status entries to one every ~280s.
    if (ev.type === 'status' || ev.category === 'status') {
        var lastStatus = null;
        for (var i = events.length - 1; i >= 0; i--) {
            if (events[i].type === 'status' || events[i].category === 'status') {
                lastStatus = events[i];
                break;
            }
        }
        var now = Date.now() / 1000;
        if (!lastStatus || (now - lastStatus.timestamp) >= 280) {
            events.push(ev);
            if (events.length > MAX_EVENTS) events.shift();
        }
    } else {
        events.push(ev);
        if (events.length > MAX_EVENTS) events.shift();
    }
    // Coalesce paints — a busy hub floods 200+ announce_summaries per second.
    _scheduleEventRender();
});

RS.listen('event_log', function(batch) {
    events = events.concat(batch);
    if (events.length > MAX_EVENTS) events = events.slice(-MAX_EVENTS);
    renderLog();
    if (typeof renderCockpitEvents === 'function') renderCockpitEvents();
});

function _remapOpStatus(step) {
    return step || '';
}

RS.listen('node_operation_status', function(data) {
    if (!data || typeof data !== 'object') return;
    var displayStep = _remapOpStatus(data.step);
    var prefixMap = {
        'add_lora': 'Adding LoRa to',
        'update_lora': 'Updating LoRa on',
        'remove_lora': 'Removing LoRa from',
        'enable_auto': 'Enabling Local Network on',
        'disable_auto': 'Disabling Local Network on',
        'add_tcp': 'Connecting',
        'update_tcp': 'Updating',
        'remove_tcp': 'Disconnecting',
        'add_server': 'Starting server on',
        'update_server': 'Updating server on',
        'remove_server': 'Stopping server on',
        'add_backbone': 'Connecting',
        'update_backbone': 'Updating',
        'remove_backbone': 'Disconnecting',
        'add_backbone_server': 'Starting Backbone server on',
        'update_backbone_server': 'Updating Backbone server on',
        'remove_backbone_server': 'Stopping Backbone server on',
        'pause_interface': 'Pausing',
        'resume_interface': 'Resuming',
        'transport': 'Updating',
        'enable_ble_peer': 'Enabling Bluetooth Peer on',
        'disable_ble_peer': 'Disabling Bluetooth Peer on',
    };
    var prefix = prefixMap[data.operation] || data.operation;
    var nodeName = data.node ? friendlyNode(data.node) : '?';

    if (data.done && data.operation === 'transport') {
        var select = document.getElementById('transport-mode-select');
        if (select) select.disabled = false;
    }

    var progressDialog = window._activeProgressDialog;
    var progressHandling = progressDialog && progressDialog.isOpen();

    if (progressHandling) {
        if (data.done) {
            if (data.error) {
                var detail = (typeof data.error === 'string') ? data.error : '';
                progressDialog.error(detail ? (displayStep + ': ' + detail) : displayStep);
            } else {
                progressDialog.success(displayStep);
            }
            window._activeProgressDialog = null;
        } else {
            progressDialog.update(displayStep);
        }
    }

    if (data.done && !progressHandling) {
        var toastColor = data.error ? 'toast-red' : 'toast-green';
        var toastDuration = data.error ? 8000 : 5000;
        showToast(displayStep, toastColor, toastDuration);

        if (data.operation === 'add_lora' && !data.error) {
            closeRnodeModal();
        }
        if (data.operation === 'resume_interface' && typeof clearConnectPublicPending === 'function') {
            clearConnectPublicPending();
        }
    }
    if (data.done && progressHandling) {
        if (data.operation === 'add_lora' && !data.error) {
            closeRnodeModal();
        }
    }

    var hubModalVisible = document.getElementById('node-modal').classList.contains('open');
    var modalStatus = document.getElementById('modal-op-status');
    if (hubModalVisible && modalStatus) {
        if (data.done) {
            modalStatus.textContent = displayStep;
            modalStatus.className = 'modal-op-status ' + (data.error ? 'error' : 'success');
            document.querySelectorAll('#node-modal .danger-btn-sm').forEach(function(btn) {
                btn.disabled = false;
            });
            setTimeout(function() {
                if (typeof loadHubInterfaces === 'function') loadHubInterfaces();
                modalStatus.style.display = 'none';
                if (typeof loadSettingsInterfacesWithRetry === 'function') loadSettingsInterfacesWithRetry(3);
                if (typeof renderMergedConnections === 'function') renderMergedConnections();
            }, 2000);
        } else {
            modalStatus.style.display = 'block';
            modalStatus.textContent = displayStep;
            modalStatus.className = 'modal-op-status active';
        }
    }

    var networkView2 = document.getElementById('view-network');
    var settingsOpStatus = document.getElementById('settings-op-status');
    if (networkView2 && networkView2.classList.contains('active') && settingsOpStatus && !progressHandling) {
        if (data.done) {
            settingsOpStatus.textContent = displayStep;
            settingsOpStatus.className = 'settings-op-status ' + (data.error ? 'error' : 'success');
            document.querySelectorAll('#panel-settings-connections .danger-btn-sm').forEach(function(btn) {
                btn.disabled = false;
            });
            // Delay + retry rides out hub-restart timing.
            setTimeout(function() {
                if (typeof loadSettingsInterfacesWithRetry === 'function') loadSettingsInterfacesWithRetry(3);
                if (typeof renderMergedConnections === 'function') renderMergedConnections();
                settingsOpStatus.style.display = 'none';
                settingsOpStatus.className = 'settings-op-status';
            }, 2000);
        } else {
            settingsOpStatus.style.display = 'block';
            settingsOpStatus.textContent = displayStep;
            settingsOpStatus.className = 'settings-op-status active';
        }
    }
    if (data.done && progressHandling) {
        setTimeout(function() {
            if (typeof loadSettingsInterfacesWithRetry === 'function') loadSettingsInterfacesWithRetry(3);
            if (typeof renderMergedConnections === 'function') renderMergedConnections();
        }, 2000);
    }

    var hostModalVisible = document.getElementById('host-modal').classList.contains('open');
    if (hostModalVisible && (data.operation === 'add_server' || data.operation === 'update_server')) {
        var hostSubmitBtn = document.getElementById('host-submit-btn');
        if (hostSubmitBtn) {
            var hostUpdating = data.operation === 'update_server';
            if (data.done) {
                if (data.error) {
                    hostSubmitBtn.textContent = 'Failed';
                    hostSubmitBtn.className = 'nr-btn nr-btn-error';
                    setTimeout(function() {
                        hostSubmitBtn.textContent = hostUpdating ? 'Save Changes' : 'Start Hosting';
                        hostSubmitBtn.className = 'nr-btn';
                        hostSubmitBtn.disabled = false;
                    }, 3000);
                } else {
                    hostSubmitBtn.textContent = hostUpdating ? 'Saved!' : 'Started!';
                    hostSubmitBtn.className = 'nr-btn nr-btn-success';
                    setTimeout(function() {
                        closeHostModal();
                        hostSubmitBtn.textContent = 'Start Hosting';
                        hostSubmitBtn.className = 'nr-btn';
                        hostSubmitBtn.disabled = false;
                    }, 1500);
                }
            } else {
                hostSubmitBtn.textContent = displayStep;
            }
        }
    }

    var backboneHostVisible = document.getElementById('backbone-host-modal');
    backboneHostVisible = backboneHostVisible && backboneHostVisible.classList.contains('open');
    if (backboneHostVisible && (data.operation === 'add_backbone_server' || data.operation === 'update_backbone_server')) {
        var bbhSubmit = document.getElementById('backbone-host-submit-btn');
        if (bbhSubmit) {
            var bbhUpdating = data.operation === 'update_backbone_server';
            if (data.done) {
                if (data.error) {
                    bbhSubmit.textContent = 'Failed';
                    bbhSubmit.className = 'nr-btn nr-btn-error';
                    setTimeout(function() {
                        bbhSubmit.textContent = bbhUpdating ? 'Save Changes' : 'Start Hosting';
                        bbhSubmit.className = 'nr-btn';
                        bbhSubmit.disabled = false;
                    }, 3000);
                } else {
                    bbhSubmit.textContent = bbhUpdating ? 'Saved!' : 'Started!';
                    bbhSubmit.className = 'nr-btn nr-btn-success';
                    setTimeout(function() {
                        if (typeof closeBackboneHostModal === 'function') closeBackboneHostModal();
                        bbhSubmit.textContent = 'Start Hosting';
                        bbhSubmit.className = 'nr-btn';
                        bbhSubmit.disabled = false;
                    }, 1500);
                }
            } else {
                bbhSubmit.textContent = displayStep;
            }
        }
    }

    // Shared connect modal handles add_tcp and add_backbone identically.
    var connectModalVisible = document.getElementById('connect-modal').classList.contains('open');
    if (connectModalVisible && (data.operation === 'add_tcp' || data.operation === 'add_backbone' || data.operation === 'update_tcp' || data.operation === 'update_backbone')) {
        if (typeof _clearConnectTimeout === 'function') _clearConnectTimeout();
        var submitBtn = document.getElementById('connect-submit-btn');
        if (submitBtn) {
            var connectUpdating = data.operation === 'update_tcp' || data.operation === 'update_backbone';
            if (data.done) {
                if (data.error) {
                    if (typeof clearConnectPublicPending === 'function') clearConnectPublicPending();
                    submitBtn.textContent = 'Failed';
                    submitBtn.className = 'nr-btn nr-btn-error w-full mt-4';
                    setTimeout(function() {
                        if (typeof _setConnectSubmitBase === 'function') {
                            _setConnectSubmitBase(submitBtn, connectUpdating ? 'Save Changes' : 'Connect');
                        } else {
                            submitBtn.textContent = connectUpdating ? 'Save Changes' : 'Connect';
                            submitBtn.className = 'nr-btn w-full mt-4';
                            submitBtn.disabled = false;
                        }
                    }, 3000);
                } else {
                    submitBtn.textContent = connectUpdating ? 'Saved!' : 'Connected!';
                    submitBtn.className = 'nr-btn nr-btn-success w-full mt-4';
                    setTimeout(function() {
                        closeConnectModal();
                        if (typeof _setConnectSubmitBase === 'function') {
                            _setConnectSubmitBase(submitBtn, 'Connect');
                        } else {
                            submitBtn.textContent = 'Connect';
                            submitBtn.className = 'nr-btn w-full mt-4';
                            submitBtn.disabled = false;
                        }
                    }, 1500);

                    if (!connectUpdating) {
                        connectionWatcher = {
                            startTime: Date.now(),
                            initialPathCount: lastStats && lastStats.path_table ? lastStats.path_table.length : 0,
                            initialAnnounceCount: typeof announceCache !== 'undefined' ? announceCache.length : 0,
                            shown: false,
                        };
                        setTimeout(function() {
                            if (connectionWatcher && !connectionWatcher.shown) {
                                showConnectionSummary();
                            }
                        }, 10000);
                    }
                }
            } else {
                submitBtn.textContent = displayStep;
            }
        }
    }

});

RS.listen('hub_interfaces_update', function(data) {
    window._hubInterfacesCached = true;
    // Pre-submit UX (handoff hints, duplicate detection) uses the full payload.
    if (typeof applyNetworkInterfacePayload === 'function') {
        applyNetworkInterfacePayload(data, { render: isViewActive('network') });
    } else {
        window._hubInterfacesData = data || null;
    }
    window._autoEnabled = !!(data && data.auto && data.auto.some(function(entry) {
        var enabled = entry && entry.enabled;
        if (enabled === undefined || enabled === null) enabled = entry && entry.interface_enabled;
        if (enabled === undefined || enabled === null) return true;
        return !/^(false|no|0|off)$/i.test(String(enabled).trim());
    }));
    if (typeof updateFirstRunInterfaceHintGate === 'function') {
        updateFirstRunInterfaceHintGate(data);
    }
    if (typeof updateAutoToggle === 'function') updateAutoToggle();
    if (isViewActive('network')) {
        if (typeof applyNetworkInterfacePayload !== 'function' && typeof renderMergedConnections === 'function') {
            renderMergedConnections();
        }
    } else {
        markViewDirty('network');
    }
    if (isViewActive('settings') && typeof loadSettingsInterfaces === 'function') {
        loadSettingsInterfaces();
    }
});

RS.listen('ble_peer_status_update', function(data) {
    window._blePeerEnabled = !!(data && data.enabled);
    if (typeof updateBlePeerToggle === 'function') updateBlePeerToggle();
    // Drop per-peer cache on disable so section reverts to "No active peers".
    if (!window._blePeerEnabled) {
        window._blePeers = {};
        window._blePeersByIdentity = {};
        window._bleOrphanIdentities = [];
        window._blePeerState = null;
        window._blePeerCount = 0;
        // Re-enable should start clean; PeripheralUnavailable re-arms on recurrence.
        window._blePeerPeripheralUnavailable = null;
        if (typeof _renderConnectionsFromCache === 'function') _renderConnectionsFromCache();
    }
    if (typeof _refreshBlePeerSectionState === 'function') _refreshBlePeerSectionState();
});

function _bleRerender() {
    if (typeof _renderConnectionsFromCache === 'function') _renderConnectionsFromCache();
    if (typeof _refreshBlePeerSectionState === 'function') _refreshBlePeerSectionState();
}

// Re-render BLE rows when an announce lands after the row is on screen,
// so display_name picks up the now-cached identity.
if (typeof PeersCache !== 'undefined' && PeersCache && typeof PeersCache.subscribe === 'function') {
    PeersCache.subscribe(function() {
        if (!window._blePeers) return;
        for (var addr in window._blePeers) {
            var p = window._blePeers[addr];
            if (p && p.connected) {
                _bleRerender();
                return;
            }
        }
    });
}

// Counts unique logical peers, deduping bidirectional connections by
// identity_hash. On Apple-without-bonding the central/peripheral identifiers
// diverge for the same physical peer, so address-only counting double-counts.
function _bleConnectedPeerCount() {
    if (typeof window._bleVisiblePeersFromCache === 'function') {
        return window._bleVisiblePeersFromCache().length;
    }
    if (!window._blePeers) return 0;
    var seenIdentities = {};
    var unidentified = 0;
    var addrs = Object.keys(window._blePeers);
    for (var i = 0; i < addrs.length; i++) {
        var p = window._blePeers[addrs[i]];
        if (!p || !p.connected) continue;
        if (p.identity_hash) {
            seenIdentities[p.identity_hash] = true;
        } else {
            unidentified += 1;
        }
    }
    return Object.keys(seenIdentities).length + unidentified;
}
window._bleConnectedPeerCount = _bleConnectedPeerCount;

// States: off | starting | on | permission_needed | bluetooth_off |
// unavailable | central_only. Fully-blocked explicit overrides win over
// 'central_only'; peripheral-failed beats backend 'on'/'starting'.
function _deriveBlePeerState() {
    var explicit = window._blePeerState;
    var blockedExplicit = explicit === 'off' || explicit === 'unavailable' ||
                          explicit === 'permission_needed' || explicit === 'bluetooth_off';
    if (explicit && explicit !== 'auto' && blockedExplicit) return explicit;
    if (window._blePeerPeripheralUnavailable && window._blePeerEnabled) return 'central_only';
    if (explicit && explicit !== 'auto') return explicit;
    var enabled = !!window._blePeerEnabled;
    var available = window._blePeerAvailable;
    var peerCount = _bleConnectedPeerCount();
    if (available === false) {
        return window._blePeerPermissionHint ? 'permission_needed' : 'unavailable';
    }
    if (!enabled) return 'off';
    return peerCount > 0 ? 'on' : 'starting';
}

function _refreshBlePeerSectionState() {
    var section = document.querySelector('.conn-section-ble');
    if (!section) return;
    var state = _deriveBlePeerState();
    var peerCount = _bleConnectedPeerCount();
    if (peerCount === 0 && typeof window._blePeerCount === 'number' && window._blePeerCount > peerCount) {
        peerCount = window._blePeerCount;
    }
    section.setAttribute('data-ble-state', state);

    var label = section.querySelector('.conn-section-label');
    if (label) {
        var labelText = ({
            'off': 'Bluetooth Peer',
            'starting': 'Bluetooth Peer \u2014 Scanning\u2026',
            'on': 'Bluetooth Peer',
            'permission_needed': 'Bluetooth Peer \u2014 Permission needed',
            'bluetooth_off': 'Bluetooth Peer \u2014 Off',
            'unavailable': 'Bluetooth Peer \u2014 Unavailable',
            'central_only': 'Bluetooth Peer \u2014 Central only',
        })[state] || 'Bluetooth Peer';
        label.textContent = labelText;
    }

    // Pill keeps the BlueZ rejection visible across re-renders (toast is ephemeral).
    var existingPill = section.querySelector('[data-ble-pill="peripheral-unavailable"]');
    if (state === 'central_only' && label) {
        var reason = window._blePeerPeripheralUnavailable || 'Peripheral mode unavailable';
        if (!existingPill) {
            var pill = document.createElement('span');
            pill.className = 'conn-iface-pill';
            pill.setAttribute('data-ble-pill', 'peripheral-unavailable');
            pill.textContent = 'Peripheral unavailable';
            label.parentNode.insertBefore(pill, label.nextSibling);
            existingPill = pill;
        }
        existingPill.setAttribute('title', reason);
    } else if (existingPill) {
        existingPill.remove();
    }

    var countBadge = section.querySelector('#conn-count-ble');
    if (countBadge) {
        if ((state === 'on' || state === 'central_only') && peerCount > 0) {
            countBadge.textContent = peerCount;
            countBadge.classList.add('has-active');
            countBadge.style.display = '';
        } else {
            countBadge.textContent = '0';
            countBadge.classList.remove('has-active');
            countBadge.style.display = (state === 'off' ? '' : 'none');
        }
    }

    var btn = section.querySelector('#conn-toggle-ble');
    if (btn) {
        // '+' to enable, kebab when the section is running (with or without
        // peers, including the central-only degraded state).
        if (state === 'on' || state === 'starting' || state === 'central_only') {
            btn.textContent = '\u22ee';
            btn.setAttribute('aria-label', 'Bluetooth Peer actions');
        } else {
            btn.textContent = '+';
            btn.setAttribute('aria-label', 'Enable Bluetooth Peer');
        }
        var unusable = (state === 'unavailable' || state === 'bluetooth_off');
        btn.disabled = unusable;
        btn.classList.toggle('disabled', unusable);
    }
}
window._refreshBlePeerSectionState = _refreshBlePeerSectionState;

RS.listen('ble_peer_discovered', function(data) {
    if (!data || !data.address) return;
    window._blePeers = window._blePeers || {};
    var existing = window._blePeers[data.address];
    if (existing) {
        existing.rssi = data.rssi;
        if (data.protocol === 'Ratspeak') existing.protocol = 'Ratspeak';
    }
    // Section is active-only; cache discovery so protocol/rssi promote instantly.
    if (!existing) {
        window._blePeers[data.address] = {
            address: data.address,
            rssi: data.rssi,
            protocol: data.protocol || 'Ratspeak',
            connected: false,
        };
    }
});

// On disconnect we stash the identity for ORPHAN_IDENTITY_TTL_MS so a fresh
// reconnect under a different BLE address (iOS RPA rotation) can adopt it
// instantly instead of waiting for the next announce.
window._bleOrphanIdentities = window._bleOrphanIdentities || [];
var ORPHAN_IDENTITY_TTL_MS = 30000;

function _pruneBleOrphanIdentities() {
    var now = Date.now();
    var src = window._bleOrphanIdentities || [];
    window._bleOrphanIdentities = src.filter(function(o) {
        return o && o.expires_at && o.expires_at > now;
    });
}

RS.listen('ble_peer_connected', function(data) {
    if (!data || !data.address) return;
    window._blePeers = window._blePeers || {};
    _pruneBleOrphanIdentities();
    var prior = window._blePeers[data.address] || {};
    var identity = data.identity_hash || prior.identity_hash || '';
    // Adopt the orphan identity only when exactly one is alive (avoids
    // mis-attribution when multiple peers rotate inside the TTL window).
    if (!identity && window._bleOrphanIdentities.length === 1) {
        identity = window._bleOrphanIdentities[0].identity_hash;
        window._bleOrphanIdentities = [];
        if (!window._blePeersByIdentity) window._blePeersByIdentity = {};
        if (!window._blePeersByIdentity[identity]) {
            window._blePeersByIdentity[identity] = {};
        }
        window._blePeersByIdentity[identity][data.address] = true;
    }
    window._blePeers[data.address] = {
        address: data.address,
        identity_hash: identity,
        protocol: data.protocol || prior.protocol || 'Ratspeak',
        rssi: prior.rssi,
        connected: true,
        connected_at: prior.connected_at || Date.now(),
    };
    _bleRerender();
});

RS.listen('ble_peer_disconnected', function(data) {
    if (!data || !data.address) return;
    if (window._blePeers && window._blePeers[data.address]) {
        var ident = window._blePeers[data.address].identity_hash;
        delete window._blePeers[data.address];
        // Drop identity from the dedup view when its last address goes.
        if (ident && window._blePeersByIdentity && window._blePeersByIdentity[ident]) {
            delete window._blePeersByIdentity[ident][data.address];
            if (Object.keys(window._blePeersByIdentity[ident]).length === 0) {
                delete window._blePeersByIdentity[ident];
                window._bleOrphanIdentities = window._bleOrphanIdentities || [];
                _pruneBleOrphanIdentities();
                window._bleOrphanIdentities.push({
                    identity_hash: ident,
                    expires_at: Date.now() + ORPHAN_IDENTITY_TTL_MS,
                });
            }
        }
        _bleRerender();
    }
});

RS.listen('ble_peer_rssi', function(data) {
    if (!data || !data.address) return;
    var peer = window._blePeers && window._blePeers[data.address];
    if (peer) peer.rssi = data.rssi;
});

// Fires once per (interface lifetime, peer address) on first signed announce.
// Dedups central + peripheral roles whose BLE addresses differ for the same
// physical device (Apple without bonding).
RS.listen('ble_peer_identity_resolved', function(data) {
    if (!data || !data.address || !data.identity_hash) return;
    window._blePeers = window._blePeers || {};
    var peer = window._blePeers[data.address];
    if (peer) {
        peer.identity_hash = data.identity_hash;
    }
    if (!window._blePeersByIdentity) {
        window._blePeersByIdentity = {};
    }
    var set = window._blePeersByIdentity[data.identity_hash];
    if (!set) {
        set = {};
        window._blePeersByIdentity[data.identity_hash] = set;
    }
    set[data.address] = true;
    _bleRerender();
});

RS.listen('ble_peer_peripheral_unavailable', function(data) {
    window._blePeerPeripheralUnavailable = (data && data.reason) || 'Peripheral mode unavailable';
    if (typeof showToast === 'function') {
        var reason = window._blePeerPeripheralUnavailable;
        showToast('Bluetooth Peer: ' + reason + ' — running as central only', 'toast-orange', 5000);
    }
    _bleRerender();
});

// Backend StatusChanged overrides JS-derived state; peer_count only used if
// it exceeds the local count (survives startup races).
RS.listen('ble_peer_status_changed', function(data) {
    if (!data) return;
    window._blePeerState = data.state;
    if (typeof data.peer_count === 'number') window._blePeerCount = data.peer_count;
    _bleRerender();
});

RS.listen('ble_scan_results', function(data) {
    var scanBtn = document.getElementById('ble-scan-btn');
    if (scanBtn) {
        scanBtn.textContent = 'Scan Again';
        scanBtn.disabled = false;
    }

    if (!data || typeof data !== 'object') return;

    if (data.error) {
        var list = document.getElementById('ble-device-list');
        if (list) {
            list.innerHTML = '<div class="ble-scan-placeholder inline-error">' +
                escapeHtml(data.error) + '</div>';
        }
        return;
    }

    var rnodeDevices = (data.devices || []).filter(function(d) {
        return d.device_type === 'rnode';
    });
    if (typeof renderBleDeviceList === 'function') {
        renderBleDeviceList(rnodeDevices);
    }
});

// Opt-in: set window._bleDiag = true in DevTools.
RS.listen('ble_diag', function(data) {
    if (window._bleDiag) window.RS.diag('log', '[ble_diag]', data && data.msg);
});

// AutoInterface JoinFailed; current producer is Apple multicast-without-entitlement.
RS.listen('auto_unavailable', function(data) {
    if (!data) return;
    window._autoUnavailable = {
        interface: data.interface || '',
        nic: data.nic || '',
        reason: data.reason || '',
        platform: data.platform || '',
        at: Date.now()
    };
    if (window._bleDiag) window.RS.diag('log', '[auto_unavailable]', data);
    // Re-render so pill appears immediately (don't wait for next stats_update).
    if (typeof refreshConnectionsList === 'function') {
        try { refreshConnectionsList(); } catch (_) {}
    }
});

// Multicast echo gained/lost per NIC. Windows-only one-shot toast points
// at the Public-profile firewall fix; other platforms only stash state.
window._autoCarrier = window._autoCarrier || {};
window._autoFirewallToastShown = false;
RS.listen('auto_carrier_state', function(data) {
    if (!data || !data.nic) return;
    var key = (data.interface || '') + ':' + data.nic;
    var prev = window._autoCarrier[key];
    window._autoCarrier[key] = {
        ok: !!data.ok,
        reason: data.reason || '',
        platform: data.platform || '',
        at: Date.now()
    };
    if (window._bleDiag) window.RS.diag('log', '[auto_carrier_state]', data);
    if (data.platform === 'windows' && !data.ok && !window._autoFirewallToastShown) {
        var prevWasOk = prev && prev.ok === true;
        var firstSinceSpawn = !prev;
        if (prevWasOk || firstSinceSpawn) {
            if (typeof showToast === 'function') {
                showToast(
                    'Local Network: no multicast echo on ' + data.nic +
                    '. Windows Defender Firewall blocks IPv6 multicast on Public Wi-Fi profiles by default. ' +
                    'In Settings → Network & Internet → Wi-Fi → properties, switch the active network to Private — ' +
                    'or run as administrator: Get-NetConnectionProfile | Set-NetConnectionProfile -NetworkCategory Private',
                    'toast-yellow',
                    12000
                );
                window._autoFirewallToastShown = true;
            }
        }
    }
});

// Linux only: BlueZ has no native passkey UI. attempt_id dedupes BlueZ SMP
// retries, dismisses stale modals on attempt bump, and auto-closes via
// ble_rnode_pairing_finished. Apple/Windows show OS dialogs themselves.
window._bleRnodePromptOpen = null;   // { attemptId, sheet, silent }
window._bleRnodeLatestAttempt = 0;   // monotonic latest attempt_id we'll honor

function _bleRnodeDismissModal(record) {
    if (!record) return;
    record.silent = true;
    if (record.sheet) {
        var cancelBtn = record.sheet.querySelector('.rs-dialog-cancel');
        if (cancelBtn) {
            try { cancelBtn.click(); } catch (_) {}
        }
    }
}

RS.listen('ble_rnode_passkey_prompt', function(data) {
    var device = (data && data.device) || 'RNode';
    var attemptId = (data && typeof data.attempt_id === 'number') ? data.attempt_id : 0;

    // Stale prompt from torn-down attempt; latest-attempt gate guards stacked modals.
    if (attemptId && attemptId < window._bleRnodeLatestAttempt) {
        return;
    }
    if (attemptId > window._bleRnodeLatestAttempt) {
        window._bleRnodeLatestAttempt = attemptId;
    }

    if (window._bleRnodePromptOpen) {
        if (window._bleRnodePromptOpen.attemptId === attemptId) {
            // BlueZ retry of the same attempt — keep existing modal.
            return;
        }
        // Older attempt's modal still up; backend already aborted, just dismiss.
        _bleRnodeDismissModal(window._bleRnodePromptOpen);
        window._bleRnodePromptOpen = null;
    }

    var record = { attemptId: attemptId, sheet: null, silent: false };
    window._bleRnodePromptOpen = record;

    rsPrompt({
        title: 'Pair with RNode',
        message: 'Enter the 6-digit passkey shown on the RNode’s OLED display. ' +
            'If the RNode has already left pairing mode, cancel here, hold the ' +
            'RNode’s left button to re-arm pairing, then add the radio again.',
        confirmText: 'Pair',
        cancelText: 'Cancel',
        placeholder: '000000'
    }).then(function(value) {
        var wasCurrent = (window._bleRnodePromptOpen === record);
        if (wasCurrent) window._bleRnodePromptOpen = null;
        // Silent dismiss (pairing_finished / newer attempt) skips cancel/submit.
        if (record.silent) return;
        if (value === null || value === undefined || value === '') {
            if (wasCurrent) RS.invoke('cancel_ble_rnode_pairing').catch(function() {});
            return;
        }
        var digits = String(value).replace(/\D+/g, '');
        var passkey = parseInt(digits, 10);
        if (!digits || isNaN(passkey) || passkey < 0 || passkey > 999999) {
            showToast('Passkey must be a 6-digit number.', 'toast-red', 4000);
            if (wasCurrent) RS.invoke('cancel_ble_rnode_pairing').catch(function() {});
            return;
        }
        RS.invoke('submit_ble_rnode_passkey', { passkey: passkey }).catch(function(err) {
            var msg = (err && err.message) || 'Failed to submit passkey';
            showToast(msg, 'toast-red', 4000);
        });
    });

    // rsPrompt mounts synchronously; capture the sheet for programmatic dismiss.
    var sheets = document.querySelectorAll('.bottom-sheet');
    record.sheet = sheets.length ? sheets[sheets.length - 1] : null;
    if (record.sheet) {
        record.sheet.setAttribute('data-ble-pair-attempt', String(attemptId));
    }
});

// Tail of linux_trigger_pairing; auto-closes modal bound to the attempt.
RS.listen('ble_rnode_pairing_finished', function(data) {
    var attemptId = (data && typeof data.attempt_id === 'number') ? data.attempt_id : 0;
    var status = (data && data.status) || '';
    if (window._bleDiag) {
        window.RS.diag('log', '[ble_rnode_pairing_finished]', attemptId, status);
    }
    if (!window._bleRnodePromptOpen) return;
    if (window._bleRnodePromptOpen.attemptId !== attemptId) return;
    var record = window._bleRnodePromptOpen;
    window._bleRnodePromptOpen = null;
    _bleRnodeDismissModal(record);
});

// Android-only: interface teardown must close the Kotlin GATT link, or the
// RNode stays connected and never advertises again.
RS.listen('ble_rnode_disconnect_native', function() {
    if (typeof window.RatspeakAndroid !== 'undefined' &&
        typeof window.RatspeakAndroid.disconnectBleDevice === 'function') {
        try { window.RatspeakAndroid.disconnectBleDevice(); } catch (_) {}
    }
});

// Android-only: Rust asks Kotlin to open GATT + TCP bridge first.
RS.listen('ble_rnode_connect_native', function(data) {
    if (typeof window.RatspeakAndroid === 'undefined' ||
        typeof window.RatspeakAndroid.connectBleDevice !== 'function') {
        RS.invoke('ble_rnode_bridge_ready', {
            args: { tcp_port: 0 }
        }).catch(function() {});
        return;
    }

    // Bonding can take a while; phase updates keep the dialog meaningful.
    var PHASE_MESSAGES = {
        starting: 'Starting BLE connection...',
        connecting: 'Connecting to RNode (GATT)...',
        connecting_retry: 'Retrying paired BLE reconnect...',
        mtu: 'Negotiating MTU...',
        discovering: 'Discovering services...',
        bonding: "Pairing...\nThis may take a moment. Don't unplug or restart your device.",
        pairing_settle: 'Pairing complete — reconnecting securely...',
        subscribing: 'Enabling notifications...',
        bridge: 'Opening TCP bridge...',
        ready: 'Connected — linking radio...',
    };
    window._onBleConnectProgress = function(phase) {
        var pd = window._activeProgressDialog;
        if (pd && pd.isOpen() && PHASE_MESSAGES[phase]) {
            pd.update(PHASE_MESSAGES[phase]);
        }
    };

    window._onBleConnectResult = function(result) {
        window._onBleConnectResult = null;
        window._onBleConnectProgress = null;
        if (result.success) {
            RS.invoke('ble_rnode_bridge_ready', {
                args: {
                    tcp_port: data.tcp_port,
                    name: data.name,
                    port: 'ble://' + data.address,
                    frequency: data.frequency,
                    bandwidth: data.bandwidth,
                    spreading_factor: data.spreading_factor,
                    coding_rate: data.coding_rate,
                    tx_power: data.tx_power,
                    mode: data.mode,
                    airtime_limit_short: data.airtime_limit_short,
                    airtime_limit_long: data.airtime_limit_long,
                }
            }).catch(function() {});
        } else {
            var errRaw = result.error || 'Unknown error';
            var pairingMode = errRaw.indexOf('ERR_PAIRING_MODE') === 0;
            var staleBond = errRaw.indexOf('ERR_STALE_BOND') === 0;
            var errMsg = pairingMode
                ? 'Pairing failed. Fresh installs are ready briefly after boot; otherwise hold P or OK on the RNode, then retry.'
                : staleBond
                    ? 'Paired BLE reconnect failed. Android may have a stale pairing for this RNode; remove it from Android Bluetooth settings, put the RNode in pairing mode, then pair again.'
                : 'BLE connect failed: ' + errRaw;
            if (typeof window.RatspeakAndroid !== 'undefined' &&
                typeof window.RatspeakAndroid.disconnectBleDevice === 'function') {
                try { window.RatspeakAndroid.disconnectBleDevice(); } catch (_) {}
            }
            if (data.rollback_on_error && data.name) {
                RS.invoke('cancel_ble_connect', { name: data.name }).catch(function() {});
            }
            var pd = window._activeProgressDialog;
            if (pd && pd.isOpen()) {
                pd.error(errMsg);
                if (pd.onClose) {
                    pd.onClose(function() {
                        if (window._activeProgressDialog === pd) window._activeProgressDialog = null;
                        if (data.rollback_on_error && typeof openRnodeModal === 'function') {
                            openRnodeModal('ble');
                        }
                    });
                }
            } else {
                showToast(errMsg, 'toast-red', 5000);
                if (data.rollback_on_error && typeof openRnodeModal === 'function') {
                    openRnodeModal('ble');
                }
            }
        }
    };

    if (window._activeProgressDialog && window._activeProgressDialog.isOpen()) {
        window._activeProgressDialog.update('Connecting to RNode...\nFresh installs may already be ready; otherwise hold P or OK to allow pairing.');
    }

    window.RatspeakAndroid.connectBleDevice(data.address, data.tcp_port);
});

RS.listen('clone_warning', function(data) {
    if (document.getElementById('clone-warning-banner')) return;

    var banner = document.createElement('div');
    banner.id = 'clone-warning-banner';
    banner.className = 'clone-warning';
    banner.innerHTML = '<span>' + escapeHtml(data.message) + '</span>' +
        '<button class="nr-btn clone-settings-btn">Go to Identity</button>' +
        '<button class="clone-dismiss" title="Dismiss">&times;</button>';

    banner.querySelector('.clone-settings-btn').addEventListener('click', function() { switchView('identity'); });
    banner.querySelector('.clone-dismiss').addEventListener('click', function() { banner.remove(); });

    var mainContent = document.querySelector('.main-content');
    if (mainContent) {
        mainContent.insertBefore(banner, mainContent.firstChild);
    }
});

RS.listen('identity_reset', function(data) {
    try {
        localStorage.removeItem('ratspeak_identity_hash');
        localStorage.removeItem('ratspeak_identity_name');
        for (var i = localStorage.length - 1; i >= 0; i--) {
            var k = localStorage.key(i);
            if (k && (k.indexOf('ratspeak_identity_') === 0 || k.indexOf('ratspeak_drafts_') === 0)) {
                localStorage.removeItem(k);
            }
        }
    } catch (_) {}
    // Drop cached peers immediately so pre-reset rows don't flash before reload.
    if (typeof PeersCache !== 'undefined' && PeersCache && typeof PeersCache.clear === 'function') {
        PeersCache.clear();
    }
    waitForServerAndReload(20, '/');
});

RS.listen('system_status', function(data) {
    if (!data || typeof data !== 'object') return;
    var wasFirstReady = !_initialConnectDone;
    if (data.status === 'ready') {
        _initialConnectDone = true;
        if (!wasFirstReady) showToast('Services ready', 'toast-green');
        if (typeof loadConversations === 'function') loadConversations();
        if (typeof loadIdentities === 'function') loadIdentities();
        return;
    }
    if (!_initialConnectDone) _initialConnectDone = true;
});

RS.listen('identity_error', function(data) {
    var msg = (data && data.error) ? data.error : 'Identity operation failed.';
    var toastClass = (data && data.degraded) ? 'toast-red' : 'toast-orange';
    var duration = (data && data.degraded) ? 12000 : 6000;
    showToast(msg, toastClass, duration);

    var switchBtns = document.querySelectorAll('.identity-switch-btn');
    switchBtns.forEach(function(btn) {
        btn.textContent = 'Select';
        btn.disabled = false;
    });
});

RS.listen('announce_triggered', function(data) {
    if (!data || data.success) return;
    // settings.js owns the manual announce UX and gives context-specific errors.
    if (data.error === 'no_interfaces' || data.error === 'not_sent' || data.error === 'not_ready') {
        return;
    }
    if (typeof showToast === 'function') {
        showToast('Announce failed: ' + (data.error || 'Unknown error'), 'toast-red');
    }
});

document.addEventListener('DOMContentLoaded', function() {
    initConnections();
});
