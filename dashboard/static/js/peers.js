var peersSort = 'name';
var peersSearch = '';
var peersCollapsedGroups = { 'online_star': true, 'offline': true };
var peersSelectedHash = null;
var _peersScrollTop = 0;
var _peersRowHeight = 36;
var _peersBufferRows = 10;
var _peersLastDirtyKey = '';
var _peersInitialized = false;
var _peersContactCache = null;
var _peersSearchTimer = null;
var _peersDataStale = true;
var _peersLastGen = -1;
var _peersAutoRefreshTimer = null;
var PEERS_AUTO_REFRESH_MS = 5000;
// Bumped on PeersCache change or path_table delta — both alter render output.
var _peersCacheGen = 0;

// rAF-coalesced renderer; scrollOnly requests upgrade to full if any
// same-frame caller asked for one.
var _peersRenderRaf = null;
var _peersPendingScrollOnly = false;

function _peerRowProfileStatus(peer) {
    if (typeof ratspeakProfileStatusText === 'function') return ratspeakProfileStatusText(peer);
    if (!peer || !peer.supports_ratspeak || !peer.profile_status) return '';
    return String(peer.profile_status).trim();
}

function _peerRowHeight(item, baseHeight, statusHeight) {
    return item && item.type === 'row' && _peerRowProfileStatus(item.data) ? statusHeight : baseHeight;
}

function _peerListMetrics(flatItems, baseHeight, statusHeight) {
    var offsets = new Array(flatItems.length);
    var heights = new Array(flatItems.length);
    var total = 0;
    for (var i = 0; i < flatItems.length; i++) {
        offsets[i] = total;
        var h = _peerRowHeight(flatItems[i], baseHeight, statusHeight);
        heights[i] = h;
        total += h;
    }
    return { offsets: offsets, heights: heights, total: total };
}

function _peerIndexAtScrollTop(metrics, scrollTop) {
    var offsets = metrics.offsets;
    var heights = metrics.heights;
    var lo = 0;
    var hi = offsets.length - 1;
    var result = offsets.length;
    while (lo <= hi) {
        var mid = Math.floor((lo + hi) / 2);
        if (offsets[mid] + heights[mid] > scrollTop) {
            result = mid;
            hi = mid - 1;
        } else {
            lo = mid + 1;
        }
    }
    return result;
}

function scheduleRenderPeersList(scrollOnly) {
    if (_peersRenderRaf == null) {
        _peersPendingScrollOnly = !!scrollOnly;
    } else if (!scrollOnly) {
        _peersPendingScrollOnly = false;
    }
    if (_peersRenderRaf != null) return;
    _peersRenderRaf = requestAnimationFrame(function() {
        _peersRenderRaf = null;
        var so = _peersPendingScrollOnly;
        _peersPendingScrollOnly = false;
        renderPeersList(so);
    });
}

var PEERS_STATUS_ORDER = { reachable: 0, direct: 0, stale: 1, offline: 2, unreachable: 3, unknown: 4 };

// Compact iface badge labels; full names like `rns.ratspeak.org:4242` blow
// out the row width on mobile. Unknown values fall through to the raw value.
function ifaceShortLabel(name) {
    if (!name) return '';
    var lower = String(name).toLowerCase();
    if (lower.indexOf('ble_peer') === 0 || lower.indexOf('ble peer') === 0 || lower.indexOf('ble mesh') === 0 || lower.indexOf('bluetooth peer') === 0) return 'BLE';
    if (lower.indexOf('ble_rnode') === 0 || lower.indexOf('rnode_ble') === 0) return 'LoRa (BLE)';
    if (lower.indexOf('rnode') === 0 || lower.indexOf('serial') === 0 || lower.indexOf('kiss') === 0) return 'LoRa';
    if (lower.indexOf('androidusb') === 0 || lower.indexOf('android_usb') === 0) return 'LoRa (USB)';
    if (lower.indexOf('local') === 0 || lower.indexOf('shared') === 0) return 'Local';
    if (lower.indexOf('udp') === 0) return 'UDP';
    // Listed before the TCP catch-all so "Backbone to host:port" doesn't
    // fall through to TCP via the colon-substring branch below.
    if (lower.indexOf('backbone') === 0) return 'BB';
    if (lower.indexOf('tcp') === 0 || lower.indexOf(':') > 0) return 'TCP';
    return name;
}

function enrichPeersFromCache() {
    return (typeof PeersCache !== 'undefined' && PeersCache) ? PeersCache.enriched() : [];
}

function initPeersView() {
    var container = document.getElementById('peers-list-body');

    if (!_peersInitialized) {
        _peersInitialized = true;

        if (typeof PeersCache !== 'undefined' && PeersCache && typeof PeersCache.init === 'function') {
            PeersCache.init().catch(function() {});
            PeersCache.subscribe(function() {
                _peersCacheGen++;
                _peersDataStale = true;
                var v = document.getElementById('view-peers');
                if (v && v.classList.contains('active') && !document.hidden) {
                    _peersLastDirtyKey = '';
                    scheduleRenderPeersList();
                    if (peersSelectedHash) renderPeersDetailPanel(peersSelectedHash);
                }
            });
        }

        var searchInput = document.getElementById('peers-search');
        if (searchInput) {
            searchInput.addEventListener('input', function() {
                clearTimeout(_peersSearchTimer);
                var val = this.value.toLowerCase().trim();
                _peersSearchTimer = setTimeout(function() {
                    peersSearch = val;
                    _peersLastDirtyKey = '';
                    scheduleRenderPeersList();
                }, 150);
            });
            searchInput.addEventListener('keydown', function(e) {
                if (e.key === 'Enter') { e.preventDefault(); this.blur(); }
            });
        }

        var sortBtn = document.getElementById('peers-sort-btn');
        var sortMenu = document.getElementById('peers-sort-menu');
        if (sortBtn && sortMenu) {
            sortBtn.addEventListener('click', function(e) {
                e.stopPropagation();
                var isOpen = sortMenu.classList.toggle('open');
                if (isOpen) {
                    var rect = sortBtn.getBoundingClientRect();
                    sortMenu.style.top = (rect.bottom + 4) + 'px';
                    sortMenu.style.right = (window.innerWidth - rect.right) + 'px';
                }
            });
            sortMenu.querySelectorAll('.toolbar-dropdown-item').forEach(function(item) {
                item.addEventListener('click', function() {
                    peersSort = this.dataset.sort;
                    sortMenu.querySelectorAll('.toolbar-dropdown-item').forEach(function(i) { i.classList.remove('active'); });
                    this.classList.add('active');
                    sortMenu.classList.remove('open');
                    _peersLastDirtyKey = '';
                    scheduleRenderPeersList();
                });
            });
            document.addEventListener('click', function() { sortMenu.classList.remove('open'); });
            var scrollEl2 = document.getElementById('peers-list-scroll');
            if (scrollEl2) scrollEl2.addEventListener('scroll', function() { sortMenu.classList.remove('open'); }, { passive: true });
        }

        var scrollEl = document.getElementById('peers-list-scroll');
        if (scrollEl) {
            scrollEl.addEventListener('scroll', function() {
                _peersScrollTop = this.scrollTop;
                scheduleRenderPeersList(true);
            }, { passive: true });
            if (!scrollEl._ptrAttached) {
                RS.gestures.attachPullToRefresh(scrollEl, { onRefresh: refreshPeersList });
            }
        }

        if (container) {
            container.addEventListener('click', function(e) {
                var header = e.target.closest('.conn-group-header');
                if (header) {
                    peersCollapsedGroups[header.dataset.group] = !peersCollapsedGroups[header.dataset.group];
                    _peersLastDirtyKey = '';
                    scheduleRenderPeersList();
                    return;
                }
                var row = e.target.closest('.peers-row');
                if (row) {
                    var hash = row.dataset.hash;
                    if (window.innerWidth <= 768) {
                        showPeersDetailSheet(hash);
                    } else {
                        if (peersSelectedHash === hash) {
                            peersSelectedHash = null;
                            hidePeersDetail();
                        } else {
                            peersSelectedHash = hash;
                            _peersLastDirtyKey = '';
                            scheduleRenderPeersList();
                            renderPeersDetailPanel(hash);
                        }
                    }
                }
            });
        }

        resetPeersAutoRefresh();
    }

    if (_peersDataStale) {
        _peersDataStale = false;
        _peersLastDirtyKey = '';
        scheduleRenderPeersList();
    }
}

function renderPeersList(scrollOnly) {
    var container = document.getElementById('peers-list-body');
    var scrollContainer = document.getElementById('peers-list-scroll');
    if (!container || !scrollContainer) return;
    if (typeof PeersCache === 'undefined' || !PeersCache) return;
    var mobileRows = window.innerWidth <= 768;
    var baseRowHeight = mobileRows ? 58 : 36;
    var statusRowHeight = mobileRows ? 68 : 48;
    _peersRowHeight = baseRowHeight;

    if (!scrollOnly) {
        var dirtyKey = peersSearch + '|' + peersSort + '|' + JSON.stringify(peersCollapsedGroups) + '|' + peersSelectedHash + '|' + _peersCacheGen;
        if (dirtyKey === _peersLastDirtyKey) return;
        _peersLastDirtyKey = dirtyKey;
        _peersLastGen = _peersCacheGen;
    }

    var peers = enrichPeersFromCache();

    if (peers.length === 0 && !scrollOnly) {
        container.innerHTML = '<div class="empty-state">' +
            '<svg class="empty-state-svg" width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/><path d="M23 21v-2a4 4 0 0 0-3-3.87"/><path d="M16 3.13a4 4 0 0 1 0 7.75"/></svg>' +
            '<span class="empty-state-primary">No peers yet</span>' +
            '<span class="empty-state-hint">Connect to a network to discover peers</span>' +
            '</div>';
        return;
    }

    var filtered = peers;
    if (peersSearch) {
        filtered = filtered.filter(function(c) {
            var name = (c.display_name || '').toLowerCase();
            var hash = (c.hash || '').toLowerCase();
            var statusText = _peerRowProfileStatus(c).toLowerCase();
            return name.indexOf(peersSearch) >= 0 || hash.indexOf(peersSearch) >= 0 || statusText.indexOf(peersSearch) >= 0;
        });
    }

    filtered.sort(function(a, b) {
        if (peersSort === 'name') {
            var na = (a.display_name || a.hash || '').toLowerCase();
            var nb = (b.display_name || b.hash || '').toLowerCase();
            var aLetter = na.length > 0 && na[0] >= 'a' && na[0] <= 'z';
            var bLetter = nb.length > 0 && nb[0] >= 'a' && nb[0] <= 'z';
            if (aLetter !== bLetter) return aLetter ? -1 : 1;
            return na.localeCompare(nb);
        }
        if (peersSort === 'hops') {
            var ha = a.hops !== null && a.hops !== undefined ? a.hops : 999;
            var hb = b.hops !== null && b.hops !== undefined ? b.hops : 999;
            return ha - hb;
        }
        if (peersSort === 'last_seen') {
            // Prefer persisted last_seen; fall back to transient path_age.
            var lsa = a.last_seen != null ? a.last_seen : -Infinity;
            var lsb = b.last_seen != null ? b.last_seen : -Infinity;
            if (lsa !== lsb) return lsb - lsa;
            var ta = a.path_age != null ? a.path_age : 999999;
            var tb = b.path_age != null ? b.path_age : 999999;
            return ta - tb;
        }
        return 0;
    });

    var contactItems = [], onlineItems = [], onlineStarItems = [], staleItems = [], offlineItems = [];
    filtered.forEach(function(c) {
        if (c.is_contact) contactItems.push(c);
        else if (c.status === 'reachable' || c.status === 'direct') {
            if (typeof hasRealDisplayName === 'function' && hasRealDisplayName(c)) onlineItems.push(c);
            else onlineStarItems.push(c);
        } else if (c.status === 'stale') staleItems.push(c);
        else offlineItems.push(c);
    });

    var flatItems = [];
    var groupDefs = [
        { items: contactItems, key: 'contacts', label: 'Contacts' },
        { items: onlineItems, key: 'online', label: 'Recent' },
        { items: onlineStarItems, key: 'online_star', label: 'Recent*' },
        { items: staleItems, key: 'stale', label: 'Seen today' },
        { items: offlineItems, key: 'offline', label: 'Older / unknown' }
    ];
    groupDefs.forEach(function(g) {
        if (g.items.length > 0) {
            flatItems.push({ type: 'header', group: g.key, label: g.label, count: g.items.length });
            if (!peersCollapsedGroups[g.key]) {
                g.items.forEach(function(c) { flatItems.push({ type: 'row', data: c }); });
            }
        }
    });

    if (filtered.length === 0 && !scrollOnly) {
        container.innerHTML = '<div class="dashboard-peers-empty" style="padding:32px;">No peers match your filter</div>';
        return;
    }

    // Virtualized: only visible window + buffer is in the DOM, spacer divs
    // preserve total scroll height. Avoids rebuilding ~1.6k rows per poll on mobile.
    var viewportHeight = scrollContainer.clientHeight;
    var metrics = _peerListMetrics(flatItems, baseRowHeight, statusRowHeight);
    if (viewportHeight < 50) viewportHeight = Math.max(metrics.total, 500);
    var totalHeight = metrics.total;

    var startIdx = Math.max(0, _peerIndexAtScrollTop(metrics, _peersScrollTop) - _peersBufferRows);
    startIdx = Math.min(startIdx, flatItems.length);
    var endIdx = Math.min(flatItems.length, _peerIndexAtScrollTop(metrics, _peersScrollTop + viewportHeight) + _peersBufferRows);
    if (endIdx < startIdx) endIdx = startIdx;

    var html = '';
    // flex-shrink:0 — #peers-list-body is a flex column on mobile.
    var startOffset = startIdx < flatItems.length ? metrics.offsets[startIdx] : totalHeight;
    if (startIdx > 0) html += '<div style="height:' + startOffset + 'px;flex-shrink:0"></div>';
    html += buildPeersHTML(flatItems, startIdx, endIdx, metrics.heights);
    var endOffset = endIdx < flatItems.length ? metrics.offsets[endIdx] : totalHeight;
    var remaining = totalHeight - endOffset;
    if (remaining > 0) html += '<div style="height:' + remaining + 'px;flex-shrink:0"></div>';

    container.innerHTML = html;
}

function buildPeersHTML(flatItems, start, end, rowHeights) {
    var html = '';
    for (var i = start; i < end; i++) {
        var item = flatItems[i];
        var rowHeight = rowHeights && rowHeights[i] ? rowHeights[i] : _peersRowHeight;
        if (item.type === 'header') {
            var isCollapsed = peersCollapsedGroups[item.group];
            var chevron = isCollapsed ? '&#9654;' : '&#9660;';
            html += '<div class="conn-group-header" data-group="' + item.group + '" style="height:' + rowHeight + 'px;flex-shrink:0">' +
                '<span class="conn-group-chevron">' + chevron + '</span>' +
                '<span class="conn-group-label">' + item.label + '</span>' +
                '<span class="conn-group-count">(' + item.count + ')</span>' +
            '</div>';
        } else {
            var c = item.data;
            var isSelected = peersSelectedHash === c.hash;
            var displayName = c.display_name || (typeof shortHash === 'function' ? shortHash(c.hash, 8, 4) : c.hash);
            if (displayName.length > 40) displayName = displayName.substring(0, 40) + '\u2026';
            var hasName = c.display_name && c.display_name !== '' && c.display_name !== c.hash;
            var nameClass = 'peers-row-name' + (hasName ? '' : ' is-hash');
            var statusClass = 'status-' + c.status;
            var avatarSize = window.innerWidth <= 768 ? 44 : 28;
            var av = (typeof identityAvatar === 'function') ? identityAvatar(c.hash, avatarSize) : '';
            var ifaceLabel = c.iface_is_live ? ifaceShortLabel(c.iface) : '';
            var ifaceBadge = ifaceLabel
                ? '<span class="peers-iface-badge" title="' + escapeHtml('Live via ' + (c.iface || '')) + '">'
                  + escapeHtml(ifaceLabel) + '</span>'
                : '';
            var profileStatus = _peerRowProfileStatus(c);
            var statusHtml = profileStatus
                ? '<span class="peers-row-status" title="' + escapeHtml(profileStatus) + '">' + escapeHtml(profileStatus) + '</span>'
                : '';

            html += '<div class="peers-row' + (isSelected ? ' selected' : '') + (profileStatus ? ' has-profile-status' : '') + '" data-hash="' + escapeHtml(c.hash) + '" style="height:' + rowHeight + 'px;flex-shrink:0">' +
                '<span class="conn-status-dot ' + statusClass + '"></span>' +
                '<div class="peers-row-avatar">' + av + '</div>' +
                '<span class="peers-row-main">' +
                    '<span class="' + nameClass + '">' + ratspeakDisplayNameHtml(displayName, c) + '</span>' +
                    statusHtml +
                '</span>' +
                '<span class="peers-row-meta">' +
                    ifaceBadge +
                '</span>' +
            '</div>';
        }
    }
    return html;
}

function renderPeersDetailPanel(hash) {
    var panel = document.getElementById('peers-detail-panel');
    var content = document.getElementById('peers-detail-content');
    var empty = document.getElementById('peers-detail-empty');
    if (!panel || !content) return;

    var enriched = enrichPeersFromCache();
    var peer = null;
    for (var ei = 0; ei < enriched.length; ei++) {
        if (enriched[ei].hash === hash) { peer = enriched[ei]; break; }
    }
    if (!peer) { hidePeersDetail(); return; }

    var iface = peer.iface || null;

    var hasName = peer.display_name && peer.display_name !== '' && peer.display_name !== peer.hash;
    var displayName = hasName ? peer.display_name : (typeof shortHash === 'function' ? shortHash(peer.hash, 8, 4) : peer.hash.substring(0, 12) + '\u2026');
    var av = (typeof identityAvatar === 'function') ? identityAvatar(hash, 64) : '';
    var statusLabel = peer.status === 'reachable' || peer.status === 'direct' ? 'Recent' : peer.status === 'stale' ? 'Seen today' : 'Older / unknown';
    var statusDotClass = 'status-' + peer.status;
    var lastHeardText = typeof formatLastHeard === 'function' ? formatLastHeard(peer.last_seen) : (peer.last_seen ? new Date(peer.last_seen * 1000).toLocaleString() : 'No activity yet');
    var firstHeardText = peer.first_seen ? (typeof formatLastHeard === 'function' ? formatLastHeard(peer.first_seen) : new Date(peer.first_seen * 1000).toLocaleString()) : '\u2014';
    var pathAgeText = peer.in_path && typeof formatPathAge === 'function' ? formatPathAge(peer.path_age) : '\u2014';
    var viaText = peer.in_path ? (typeof formatVia === 'function' ? formatVia(peer.via) : (peer.via || 'direct')) : '\u2014';
    var hopText = (peer.hops !== null && peer.hops !== undefined) ? peer.hops : '\u2014';
    var ifaceText = iface ? iface + (peer.iface_is_live ? '' : ' (last known)') : '\u2014';

    var html = '<div class="peers-detail-header">' +
        '<div class="peers-detail-avatar">' + av + '</div>' +
        '<div class="peers-detail-name">' + ratspeakDisplayNameHtml(displayName, peer) + '</div>' +
        '<div class="peers-detail-hash" id="peers-detail-hash-copy" title="Click to copy">' + escapeHtml(hash) + '</div>' +
        '<div class="peers-detail-status"><span class="conn-status-dot ' + statusDotClass + '"></span> ' + statusLabel + '</div>' +
    '</div>';

    html += '<div class="peers-detail-section">' +
        '<div class="peers-detail-section-title">Activity</div>' +
        '<div class="peers-detail-field"><span class="peers-detail-field-label">Last heard</span><span class="peers-detail-field-value">' + escapeHtml(lastHeardText) + '</span></div>' +
        '<div class="peers-detail-field"><span class="peers-detail-field-label">First heard</span><span class="peers-detail-field-value">' + escapeHtml(firstHeardText) + '</span></div>' +
    '</div>';

    html += '<div class="peers-detail-section">' +
        '<div class="peers-detail-section-title">Routing</div>' +
        '<div class="peers-detail-field"><span class="peers-detail-field-label">Route</span><span class="peers-detail-field-value">' + escapeHtml(peer.route_label || 'No current path') + '</span></div>' +
        '<div class="peers-detail-field"><span class="peers-detail-field-label">Hops</span><span class="peers-detail-field-value">' + hopText + '</span></div>' +
        '<div class="peers-detail-field"><span class="peers-detail-field-label">Path age</span><span class="peers-detail-field-value">' + pathAgeText + '</span></div>' +
        '<div class="peers-detail-field"><span class="peers-detail-field-label">Via</span><span class="peers-detail-field-value">' + escapeHtml(viaText) + '</span></div>' +
        '<div class="peers-detail-field"><span class="peers-detail-field-label">Interface</span><span class="peers-detail-field-value">' + escapeHtml(ifaceText) + '</span></div>' +
    '</div>';

    html += '<div class="peers-detail-section" id="peers-detail-contact-section">' +
        '<div class="peers-detail-section-title">Contact</div>' +
        '<div class="peers-detail-field"><span class="peers-detail-field-label">Saved</span><span class="peers-detail-field-value">' + (peer.is_contact ? 'Yes' : 'No') + '</span></div>';

    if (peer.is_contact && _peersContactCache) {
        var contact = null;
        _peersContactCache.forEach(function(ct) { if (ct.dest_hash === hash || ct.hash === hash) contact = ct; });
        if (contact) {
            if (contact.trust) html += '<div class="peers-detail-field"><span class="peers-detail-field-label">Trust</span><span class="peers-detail-field-value">' + escapeHtml(contact.trust) + '</span></div>';
        }
    }
    html += '</div>';

    html += '<div class="peers-detail-actions entity-action-grid">';
    if (typeof voiceActionButtonHtml === 'function') {
        html += voiceActionButtonHtml('peers-detail-call-btn', hash);
    }
    html += '<button class="nr-btn entity-action-btn" id="peers-detail-message-btn"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/></svg><span>Message</span></button>';
    if (!peer.is_contact) {
        html += '<button class="nr-btn entity-action-btn" id="peers-detail-add-btn"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M16 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2"/><circle cx="8.5" cy="7" r="4"/><line x1="20" y1="8" x2="20" y2="14"/><line x1="23" y1="11" x2="17" y2="11"/></svg><span>Add</span></button>';
    }
    html += '<button class="danger-btn entity-action-btn" id="peers-detail-block-btn"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="4.93" y1="4.93" x2="19.07" y2="19.07"/></svg><span>Block</span></button>';
    html += '</div>';

    if (empty) empty.style.display = 'none';
    content.style.display = '';
    content.innerHTML = html;

    var hashEl = document.getElementById('peers-detail-hash-copy');
    if (hashEl) {
        hashEl.addEventListener('click', function() {
            navigator.clipboard.writeText(hash).then(function() {
                if (typeof showCopyConfirmationToast === 'function') showCopyConfirmationToast('Address');
            });
        });
    }
    var msgBtn = document.getElementById('peers-detail-message-btn');
    if (msgBtn) {
        msgBtn.addEventListener('click', function() {
            if (typeof openConversationWith === 'function') openConversationWith(hash);
        });
    }
    if (typeof wireVoiceActionButton === 'function') {
        wireVoiceActionButton('peers-detail-call-btn');
    }
    var addBtn = document.getElementById('peers-detail-add-btn');
    if (addBtn) {
        addBtn.addEventListener('click', function() {
            var prefill = peer.display_name || '';
            rsPrompt({ message: 'Contact name (optional):', placeholder: 'Display name', defaultValue: prefill }).then(function(name) {
                if (name === null) return;
                RS.invoke('add_contact', { args: { hash: hash, display_name: name.trim() || null } }).catch(function() {});
            });
        });
    }
    var blockBtn = document.getElementById('peers-detail-block-btn');
    if (blockBtn) {
        blockBtn.addEventListener('click', function() {
            var displayName = peer.display_name || (typeof shortHash === 'function' ? shortHash(hash, 8, 4) : hash.substring(0, 12));
            rsConfirmWithCheckbox({
                message: 'Block "' + displayName + '"? They won\'t appear in your peers list and their messages will be hidden.',
                danger: true,
                confirmText: 'Block',
                checkboxLabel: 'Also block at the network layer (drop their packets entirely)',
                checkboxHelp: 'Stops their messages from reaching this device at all, instead of just hiding them. Useful for relay nodes. This affects all accounts on this device.',
                defaultChecked: false
            }).then(function(result) {
                if (!result.confirmed) return;
                RS.invoke('block_contact', { args: { hash: hash, escalate_to_blackhole: result.checked } })
                    .then(function(resp) {
                        if (resp && resp.blackhole_pending && typeof showToast === 'function') {
                            showToast('Blocked. Network blackhole will activate on their next announce.', 'toast-orange', 5000);
                        }
                    })
                    .catch(function() {});
                hidePeersDetail();
            });
        });
    }

    if (peer.is_contact && !_peersContactCache) {
        RS.invoke('api_contacts').then(function(contacts) {
            _peersContactCache = contacts;
            if (peersSelectedHash === hash) renderPeersDetailPanel(hash);
        }).catch(function() {});
    }
}

function hidePeersDetail() {
    var content = document.getElementById('peers-detail-content');
    var empty = document.getElementById('peers-detail-empty');
    if (content) content.style.display = 'none';
    if (empty) empty.style.display = '';
    peersSelectedHash = null;
    _peersLastDirtyKey = '';
    scheduleRenderPeersList();
}

function showPeersDetailSheet(hash) {
    if (typeof showConnectionDetailSheet === 'function') {
        showConnectionDetailSheet(hash, { progressive: true });
        return;
    }
}

function closePeersDetailSheet() {
}

function updatePeersFromStats() {
    // path_table changed; re-render even if PeersCache rows are unchanged.
    _peersCacheGen++;
    _peersDataStale = true;
    var peersView = document.getElementById('view-peers');
    if (peersView && peersView.classList.contains('active') && !document.hidden) {
        _peersLastDirtyKey = '';
        scheduleRenderPeersList();
        if (peersSelectedHash) renderPeersDetailPanel(peersSelectedHash);
    }
}

function refreshPeersList() {
    _peersLastDirtyKey = '';
    scheduleRenderPeersList();
    if (peersSelectedHash) renderPeersDetailPanel(peersSelectedHash);
    resetPeersAutoRefresh();
}

function resetPeersAutoRefresh() {
    if (_peersAutoRefreshTimer) clearInterval(_peersAutoRefreshTimer);
    _peersAutoRefreshTimer = setInterval(function() {
        var peersView = document.getElementById('view-peers');
        if (!peersView || !peersView.classList.contains('active')) return;
        if (document.hidden) return;
        _peersLastDirtyKey = '';
        scheduleRenderPeersList();
        if (peersSelectedHash) renderPeersDetailPanel(peersSelectedHash);
    }, PEERS_AUTO_REFRESH_MS);
}

RS.listen('contacts_update', function() {
    _peersContactCache = null;
    var peersView = document.getElementById('view-peers');
    if (peersView && peersView.classList.contains('active')) {
        _peersLastDirtyKey = '';
        scheduleRenderPeersList();
        if (peersSelectedHash) renderPeersDetailPanel(peersSelectedHash);
    }
});
