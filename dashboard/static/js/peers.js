var PEERS_SORT_DEFAULT = 'last_seen';
var PEERS_SORT_OPTIONS = { name: true, hops: true, last_seen: true };
var peersSort = PEERS_SORT_DEFAULT;
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
var _peersMeasuredRows = { key: '', base: 36, status: 48 };
var _peersPendingScrollAnchor = null;
var PEERS_AUTO_REFRESH_MS = 5000;
// Bumped on PeersCache change or path_table delta — both alter render output.
var _peersCacheGen = 0;

// rAF-coalesced renderer; scrollOnly requests upgrade to full if any
// same-frame caller asked for one.
var _peersRenderRaf = null;
var _peersPendingScrollOnly = false;

function normalizePeersSort(sort) {
    sort = String(sort || '').trim();
    return PEERS_SORT_OPTIONS[sort] ? sort : PEERS_SORT_DEFAULT;
}

function syncPeersSortMenu() {
    var sortMenu = document.getElementById('peers-sort-menu');
    if (!sortMenu) return;
    sortMenu.querySelectorAll('.toolbar-dropdown-item').forEach(function(item) {
        item.classList.toggle('active', item.dataset.sort === peersSort);
    });
}

function applyPeersSortPreference(sort, opts) {
    opts = opts || {};
    var next = normalizePeersSort(sort);
    var changed = next !== peersSort;
    peersSort = next;
    syncPeersSortMenu();
    if (changed) {
        _peersLastDirtyKey = '';
        scheduleRenderPeersList();
    }
    if (opts.persist) {
        RS.invoke('set_peers_sort', { sort: peersSort }).catch(function(err) {
            if (typeof showToast === 'function') {
                showToast('Could not save peer sort preference', 'toast-error', 3000);
            }
            window.RS.diag('warn', '[peers] failed to persist sort preference:', err);
        });
    }
}

function hydratePeersSortPreference() {
    if (!window.RS || typeof RS.invoke !== 'function') return;
    RS.invoke('api_app_settings').then(function(data) {
        if (data && data.peers_sort) {
            applyPeersSortPreference(data.peers_sort, { persist: false });
        } else {
            syncPeersSortMenu();
        }
    }).catch(function(err) {
        syncPeersSortMenu();
        window.RS.diag('warn', '[peers] failed to load sort preference:', err);
    });
}

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

function _measurePeerRowHeights(compact, scrollContainer) {
    var rootSize = window.getComputedStyle
        ? getComputedStyle(document.documentElement).fontSize
        : '16px';
    var width = Math.max(240, scrollContainer.clientWidth || 0);
    var key = (compact ? 'compact' : 'wide') + '|' + rootSize + '|' + width;
    if (_peersMeasuredRows.key === key) return _peersMeasuredRows;

    function row(withStatus) {
        var probe = document.createElement('div');
        probe.className = 'peers-row' + (withStatus ? ' has-profile-status' : '');
        probe.setAttribute('aria-hidden', 'true');
        probe.style.position = 'absolute';
        probe.style.visibility = 'hidden';
        probe.style.pointerEvents = 'none';
        probe.style.width = width + 'px';
        probe.style.height = 'auto';
        probe.style.inset = '0 auto auto -10000px';
        probe.innerHTML = '<span class="conn-status-dot status-reachable"></span>' +
            '<div class="peers-row-avatar"></div>' +
            '<span class="peers-row-main"><span class="peers-row-name">Measured peer name</span>' +
            (withStatus ? '<span class="peers-row-status">A representative profile status</span>' : '') +
            '</span><span class="peers-row-meta"></span>';
        document.body.appendChild(probe);
        var height = Math.ceil(probe.getBoundingClientRect().height);
        probe.remove();
        return height;
    }

    var minimumBase = compact ? 58 : 36;
    var minimumStatus = compact ? 68 : 48;
    _peersMeasuredRows = {
        key: key,
        base: Math.max(minimumBase, row(false)),
        status: Math.max(minimumStatus, row(true))
    };
    return _peersMeasuredRows;
}

function _capturePeerScrollAnchor() {
    if (_peersPendingScrollAnchor) return;
    var scrollContainer = document.getElementById('peers-list-scroll');
    var body = document.getElementById('peers-list-body');
    if (!scrollContainer || !body) return;
    var scrollTop = scrollContainer.scrollTop;
    var rendered = body.querySelectorAll('[data-peer-index]');
    for (var index = 0; index < rendered.length; index++) {
        var row = rendered[index];
        var height = Math.max(1, row.offsetHeight || 0);
        if (row.offsetTop + height > scrollTop) {
            _peersPendingScrollAnchor = {
                index: Number(row.dataset.peerIndex),
                ratio: Math.max(0, Math.min(1, (scrollTop - row.offsetTop) / height))
            };
            return;
        }
    }
}

function _invalidatePeerRowMetrics() {
    _capturePeerScrollAnchor();
    _peersMeasuredRows.key = '';
    _peersLastDirtyKey = '';
    scheduleRenderPeersList();
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
            syncPeersSortMenu();
            hydratePeersSortPreference();
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
                    applyPeersSortPreference(this.dataset.sort, { persist: true });
                    sortMenu.classList.remove('open');
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
                    if (isCompactLayout()) {
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
            container.addEventListener('keydown', function(e) {
                if (e.key !== 'Enter' && e.key !== ' ') return;
                var target = e.target.closest('.conn-group-header, .peers-row');
                if (!target) return;
                e.preventDefault();
                target.click();
            });
        }

        window.addEventListener('ratspeak-text-scale-changed', _invalidatePeerRowMetrics);
        window.addEventListener('resize', _invalidatePeerRowMetrics, { passive: true });
        if (document.fonts && document.fonts.ready) {
            document.fonts.ready.then(_invalidatePeerRowMetrics).catch(function() {});
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
    var mobileRows = isCompactLayout();
    var measuredRows = _measurePeerRowHeights(mobileRows, scrollContainer);
    var baseRowHeight = measuredRows.base;
    var statusRowHeight = measuredRows.status;
    _peersRowHeight = baseRowHeight;

    if (!scrollOnly) {
        var dirtyKey = peersSearch + '|' + peersSort + '|' + JSON.stringify(peersCollapsedGroups) + '|' + peersSelectedHash + '|' + _peersCacheGen + '|' + measuredRows.key;
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
        if (peersSort === 'last_seen') return PeersCache.compareByLastSeen(a, b);
        return 0;
    });

    var flatItems = RS.buildPeerGroupItems(filtered, peersCollapsedGroups, function(c) {
        return typeof hasRealDisplayName === 'function' && hasRealDisplayName(c);
    }, { olderDisplayCap: peersSearch ? 0 : RS.PEERS_OLDER_DISPLAY_CAP });

    if (filtered.length === 0 && !scrollOnly) {
        container.innerHTML = '<div class="dashboard-peers-empty" style="padding:32px;">No peers match your filter</div>';
        return;
    }

    // Virtualized: only visible window + buffer is in the DOM, spacer divs
    // preserve total scroll height. Avoids rebuilding ~1.6k rows per poll on mobile.
    var viewportHeight = scrollContainer.clientHeight;
    var metrics = _peerListMetrics(flatItems, baseRowHeight, statusRowHeight);
    if (_peersPendingScrollAnchor) {
        var anchorIndex = Math.max(0, Math.min(flatItems.length - 1, _peersPendingScrollAnchor.index));
        if (flatItems.length > 0) {
            _peersScrollTop = metrics.offsets[anchorIndex] +
                metrics.heights[anchorIndex] * _peersPendingScrollAnchor.ratio;
            scrollContainer.scrollTop = _peersScrollTop;
        }
        _peersPendingScrollAnchor = null;
    }
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
            html += '<div class="conn-group-header" role="button" tabindex="0" aria-expanded="' + (!isCollapsed) + '" data-group="' + item.group + '" data-peer-index="' + i + '" style="height:' + rowHeight + 'px;flex-shrink:0">' +
                '<span class="conn-group-chevron">' + chevron + '</span>' +
                '<span class="conn-group-label">' + item.label + '</span>' +
                '<span class="conn-group-count">(' + item.count + ')</span>' +
            '</div>';
        } else if (item.type === 'cap') {
            html += '<div class="dashboard-peers-empty text-xs" data-peer-index="' + i + '" style="height:' + rowHeight + 'px;flex-shrink:0;padding:0 var(--space-7);display:flex;align-items:center;justify-content:center">' +
                escapeHtml(item.label) +
            '</div>';
        } else {
            var c = item.data;
            var isSelected = peersSelectedHash === c.hash;
            var displayName = c.display_name || (typeof shortHash === 'function' ? shortHash(c.hash, 8, 4) : c.hash);
            var hasName = c.display_name && c.display_name !== '' && c.display_name !== c.hash;
            var nameClass = 'peers-row-name' + (hasName ? '' : ' is-hash');
            var statusClass = 'status-' + c.status;
            var avatarSize = isCompactLayout() ? 44 : 28;
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

            html += '<div class="peers-row' + (isSelected ? ' selected' : '') + (profileStatus ? ' has-profile-status' : '') + '" role="button" tabindex="0" aria-label="Open ' + escapeHtml(displayName) + ', address ' + escapeHtml(c.hash) + '" data-hash="' + escapeHtml(c.hash) + '" data-peer-index="' + i + '" style="height:' + rowHeight + 'px;flex-shrink:0">' +
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

    var hasName = peer.display_name && peer.display_name !== '' && peer.display_name !== peer.hash;
    var trust = '';
    if (peer.is_contact && _peersContactCache) {
        _peersContactCache.forEach(function(ct) { if (ct.dest_hash === hash || ct.hash === hash) trust = ct.trust || ''; });
    }

    var html = '<div class="peers-detail-header">' +
        peerAvatarHtml('peers-detail-avatar', hash, 64, peer) +
        peerNameHtml('peers-detail-name', hasName ? ratspeakDisplayNameHtml(peer.display_name, peer) : '') +
        peerAddressButtonHtml(hash) +
    '</div>' +
    peerActionPairHtml('peers-detail-actions entity-action-grid', 'peers-detail', hash) +
    peerDetailsHtml('peers-detail-rows', peer, { trust: trust }) +
    peerActionRowsHtml(peer.is_contact);

    if (empty) empty.style.display = 'none';
    content.style.display = '';
    // The panel re-renders on a timer; leave unchanged markup in place so focus and hover survive.
    if (content._rsLastHtml !== html) {
        content._rsLastHtml = html;
        content.innerHTML = html;
        syncPeerDetails(content);
        wirePeerActionPair('peers-detail', hash);
        wirePeerActionRows(content, hash, { name: hasName ? peer.display_name : '', onBlocked: hidePeersDetail });
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

RS.listen('app_settings_updated', function(data) {
    if (!data || data.peers_sort === undefined) return;
    applyPeersSortPreference(data.peers_sort, { persist: false });
});
