var PEERS_SORT_DEFAULT = 'last_seen';
var PEERS_SORT_OPTIONS = { name: true, hops: true, last_seen: true };
var PEERS_SORT_LABELS = { name: 'Name', last_seen: 'Last heard', hops: 'Hops' };
var peersSort = PEERS_SORT_DEFAULT;
var peersSearch = '';
// Unnamed active peers and Older / unknown start collapsed.
var peersCollapsedGroups = { 'online_star': true, 'offline': true };
var peersSelectedHash = null;
var _peersScrollTop = 0;
var _peersBufferRows = 10;
var _peersLastDirtyKey = '';
var _peersInitialized = false;
var _peersSearchTimer = null;
var _peersDataStale = true;
var _peersAutoRefreshTimer = null;
var _peersMeasuredRows = { key: '', base: 48, status: 60, header: 32, cap: 64, gap: 2 };
var _peersPendingScrollAnchor = null;
var PEERS_AUTO_REFRESH_MS = 5000;
// Bumped on PeersCache change or path_table delta — both alter render output.
var _peersCacheGen = 0;

// rAF-coalesced renderer; scrollOnly requests upgrade to full if any
// same-frame caller asked for one.
var _peersRenderRaf = null;
var _peersPendingScrollOnly = false;

function hasRealDisplayName(c) {
    var name = c.display_name;
    var hash = c.hash || '';
    if (!name || name === '') return false;
    if (name === hash) return false;
    if (name.length >= 12 && hash.startsWith(name)) return false;
    // Reject JSON/script payloads masquerading as display names.
    if (name.charAt(0) === '{') return false;
    return true;
}

function normalizePeersSort(sort) {
    sort = String(sort || '').trim();
    return PEERS_SORT_OPTIONS[sort] ? sort : PEERS_SORT_DEFAULT;
}

// The options button says the sort, and fills when it is not the default.
function syncPeersSortMenu() {
    var btn = document.getElementById('peers-sort-btn');
    if (!btn) return;
    var label = 'Sort, sorted by ' + PEERS_SORT_LABELS[peersSort].toLowerCase();
    btn.setAttribute('aria-label', label);
    btn.setAttribute('aria-pressed', peersSort !== PEERS_SORT_DEFAULT ? 'true' : 'false');
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

function openPeersSortMenu(trigger) {
    RS.kit.menu(trigger, {
        title: 'Sort peers',
        caption: 'Sort by',
        items: ['name', 'last_seen', 'hops'].map(function(key) {
            return {
                label: PEERS_SORT_LABELS[key],
                checked: peersSort === key,
                onSelect: function() { applyPeersSortPreference(key, { persist: true }); }
            };
        })
    });
}

function _peerRowProfileStatus(peer) {
    if (typeof ratspeakProfileStatusText === 'function') return ratspeakProfileStatusText(peer);
    if (!peer || !peer.supports_ratspeak || !peer.profile_status) return '';
    return String(peer.profile_status).trim();
}

// Slots: each item's height plus the list gap after it, so offsets match the flex layout.
function _peerItemHeight(item, rows) {
    if (item.type === 'header') return rows.header;
    if (item.type === 'cap') return rows.cap;
    return _peerRowProfileStatus(item.data) ? rows.status : rows.base;
}

function _peerListMetrics(flatItems, rows) {
    var offsets = new Array(flatItems.length);
    var heights = new Array(flatItems.length);
    var total = 0;
    for (var i = 0; i < flatItems.length; i++) {
        offsets[i] = total;
        var h = _peerItemHeight(flatItems[i], rows);
        heights[i] = h;
        total += h + rows.gap;
    }
    return { offsets: offsets, heights: heights, total: flatItems.length ? total - rows.gap : 0, gap: rows.gap };
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

// Row heights follow width, text size, and the touch sizes; measured once per layout.
function _measurePeerRowHeights(compact, body) {
    var rootSize = window.getComputedStyle
        ? getComputedStyle(document.documentElement).fontSize
        : '16px';
    var width = Math.max(240, body.clientWidth || 0);
    var key = (compact ? 'compact' : 'wide') + '|' + rootSize + '|' + width;
    if (_peersMeasuredRows.key === key) return _peersMeasuredRows;

    var probe = document.createElement('div');
    probe.className = 'ui-list';
    probe.setAttribute('aria-hidden', 'true');
    probe.style.cssText = 'position:absolute;visibility:hidden;pointer-events:none;left:-10000px;top:0;width:' + width + 'px';
    probe.innerHTML =
        '<button type="button" class="ui-caption ui-caption--toggle">' + RS.kit.icon('chevron-down') + '<span class="t">Active <span class="n">1</span></span></button>' +
        '<button type="button" class="ui-row ui-row--one">' + RS.kit.avatar('', 'compact') + '<span class="ui-row-main"><span class="ui-row-top ui-row-top--hug"><span class="ui-row-title">Measured peer</span><span class="ui-row-trail">Radio</span></span></span></button>' +
        '<button type="button" class="ui-row">' + RS.kit.avatar('', 'compact') + '<span class="ui-row-main"><span class="ui-row-top ui-row-top--hug"><span class="ui-row-title">Measured peer</span></span><span class="ui-row-sub"><span class="t">A representative status</span></span></span></button>' +
        '<p class="ui-list-note">Showing the 300 most recent of 1,120. <button type="button" class="ui-link">Search to find others</button></p>';
    var host = document.getElementById('peers-list-body') || document.body;
    host.appendChild(probe);
    var kids = probe.children;
    var gap = parseFloat(getComputedStyle(probe).rowGap) || 0;
    var noteStyle = getComputedStyle(kids[3]);
    var note = kids[3].getBoundingClientRect().height + (parseFloat(noteStyle.marginTop) || 0) + (parseFloat(noteStyle.marginBottom) || 0);
    _peersMeasuredRows = {
        key: key,
        header: Math.ceil(kids[0].getBoundingClientRect().height),
        base: Math.ceil(kids[1].getBoundingClientRect().height),
        status: Math.ceil(kids[2].getBoundingClientRect().height),
        cap: Math.ceil(note),
        gap: gap
    };
    probe.remove();
    return _peersMeasuredRows;
}

function _capturePeerScrollAnchor() {
    if (_peersPendingScrollAnchor) return;
    var scrollContainer = document.getElementById('peers-list-scroll');
    var body = document.getElementById('peers-list-body');
    if (!scrollContainer || !body) return;
    var scrollTop = scrollContainer.scrollTop;
    var rendered = body.querySelectorAll('[data-peer-index]:not(.is-pinned)');
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

function enrichPeersFromCache() {
    return (typeof PeersCache !== 'undefined' && PeersCache) ? PeersCache.enriched() : [];
}

function _syncPeersSearchClear() {
    var clear = document.getElementById('peers-search-clear');
    var input = document.getElementById('peers-search');
    if (clear && input) clear.hidden = !input.value;
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
                _syncPeersSearchClear();
                var val = this.value.toLowerCase().trim();
                _peersSearchTimer = setTimeout(function() {
                    peersSearch = val;
                    _peersLastDirtyKey = '';
                    scheduleRenderPeersList();
                }, 150);
            });
            searchInput.addEventListener('keydown', function(e) {
                if (e.key === 'Enter') { e.preventDefault(); this.blur(); }
                if (e.key === 'Escape' && this.value) { e.preventDefault(); clearPeersSearch(); }
            });
        }
        var clearBtn = document.getElementById('peers-search-clear');
        if (clearBtn) clearBtn.addEventListener('click', function() {
            clearPeersSearch();
            if (searchInput) searchInput.focus();
        });

        var sortBtn = document.getElementById('peers-sort-btn');
        if (sortBtn) {
            syncPeersSortMenu();
            hydratePeersSortPreference();
            sortBtn.addEventListener('click', function(e) {
                e.stopPropagation();
                openPeersSortMenu(sortBtn);
            });
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
                var header = e.target.closest('.ui-caption--toggle[data-group]');
                if (header) {
                    var group = header.dataset.group;
                    peersCollapsedGroups[group] = !peersCollapsedGroups[group];
                    _peersLastDirtyKey = '';
                    scheduleRenderPeersList();
                    return;
                }
                if (e.target.closest('[data-peers-search-focus]')) {
                    if (searchInput) searchInput.focus();
                    return;
                }
                if (e.target.closest('[data-peers-retry]')) {
                    if (PeersCache && PeersCache.init) PeersCache.init().catch(function() {});
                    return;
                }
                if (e.target.closest('[data-peers-clear]')) { clearPeersSearch(); return; }
                if (e.target.closest('[data-peers-network]')) { switchView('network'); return; }
                var row = e.target.closest('.ui-row[data-hash]');
                if (row) {
                    var hash = row.dataset.hash;
                    if (isCompactLayout()) {
                        openPersonSheet(hash, 'peer');
                    } else if (peersSelectedHash === hash) {
                        hidePeersDetail();
                    } else {
                        peersSelectedHash = hash;
                        _peersLastDirtyKey = '';
                        scheduleRenderPeersList();
                        renderPeersDetailPanel(hash);
                    }
                }
            });
        }
        var detail = document.getElementById('peers-detail-panel');
        if (detail) detail.addEventListener('click', function(e) {
            if (e.target.closest('[data-peers-network]')) switchView('network');
        });

        // A height-only resize (the Android keyboard) re-renders the visible window only.
        var relayoutPeers = watchViewLayout('peers', _invalidatePeerRowMetrics, function() {
            scheduleRenderPeersList(true);
        });
        window.addEventListener('ratspeak-text-scale-changed', relayoutPeers);
        if (document.fonts && document.fonts.ready) {
            document.fonts.ready.then(relayoutPeers).catch(function() {});
        }

        resetPeersAutoRefresh();
    }

    if (_peersDataStale) {
        _peersDataStale = false;
        _peersLastDirtyKey = '';
        scheduleRenderPeersList();
    }
}

function clearPeersSearch() {
    var input = document.getElementById('peers-search');
    if (input) input.value = '';
    clearTimeout(_peersSearchTimer);
    peersSearch = '';
    _syncPeersSearchClear();
    _peersLastDirtyKey = '';
    scheduleRenderPeersList();
}

function _peersFirstRunHtml() {
    return '<div class="person-onboard"><span class="ui-tile ui-tile--lg" aria-hidden="true">' + RS.kit.icon('globe') + '</span>' +
        '<h3 class="ui-empty-title">No peers yet</h3>' +
        '<p class="ui-empty-body">Peers are the people and devices your connections hear. Connect to a network to start.</p>' +
        '<div class="ui-empty-actions"><button type="button" class="ui-btn ui-btn--primary ui-btn--lg" data-peers-network>Connect to a network</button></div></div>';
}

function _peersErrorHtml() {
    return '<div class="ui-empty"><span class="ui-tile ui-tile--lg ui-tile--error" aria-hidden="true">' + RS.kit.icon('alert') + '</span>' +
        '<h3 class="ui-empty-title">Peers didn’t load</h3>' +
        '<p class="ui-empty-body">Ratspeak couldn’t read the peer list. Messages and connections still work.</p>' +
        '<div class="ui-empty-actions"><button type="button" class="ui-btn" data-peers-retry>' + RS.kit.icon('refresh') + 'Try again</button></div></div>';
}

function _setPeersChrome(count, hasList) {
    var countEl = document.getElementById('peers-count');
    if (countEl) countEl.innerHTML = hasList ? '<span class="ui-sr">, </span>' + count.toLocaleString('en-US') : '';
    var search = document.getElementById('peers-search-wrap');
    if (search) search.hidden = !hasList;
}

function _peersEmptyDetail(state) {
    var empty = document.getElementById('peers-detail-empty');
    if (!empty) return;
    var html = state === 'empty' ? _peersFirstRunHtml()
        : state === 'error' ? ''
        : '<div class="ui-empty ui-empty--quiet ui-empty--center"><h3 class="ui-empty-title">Select a peer</h3><p class="ui-empty-body">Their route, connection, and actions show here.</p></div>';
    if (empty._rsLastHtml !== html) {
        empty._rsLastHtml = html;
        empty.innerHTML = html;
    }
}

function _peersUnreadByHash() {
    var map = Object.create(null);
    if (typeof lxmfConversations === 'undefined' || !Array.isArray(lxmfConversations)) return map;
    for (var i = 0; i < lxmfConversations.length; i++) {
        var c = lxmfConversations[i];
        if (c && c.hash && c.unread > 0) map[c.hash] = c.unread;
    }
    return map;
}

function renderPeersList(scrollOnly) {
    var container = document.getElementById('peers-list-body');
    var scrollContainer = document.getElementById('peers-list-scroll');
    if (!container || !scrollContainer) return;
    if (typeof PeersCache === 'undefined' || !PeersCache) return;
    var compact = isCompactLayout();
    var measured = _measurePeerRowHeights(compact, container);

    if (!scrollOnly) {
        var dirtyKey = peersSearch + '|' + peersSort + '|' + JSON.stringify(peersCollapsedGroups) + '|' + peersSelectedHash + '|' + _peersCacheGen + '|' + measured.key;
        if (dirtyKey === _peersLastDirtyKey) return;
        _peersLastDirtyKey = dirtyKey;
    }

    var peers = enrichPeersFromCache();

    if (peers.length === 0) {
        if (scrollOnly) return;
        var failed = typeof PeersCache.loadFailed === 'function' && PeersCache.loadFailed();
        _setPeersChrome(0, false);
        container.innerHTML = failed ? _peersErrorHtml()
            : compact ? _peersFirstRunHtml()
            : '<div class="ui-empty ui-empty--quiet"><p class="ui-empty-body">Peers you hear show here.</p></div>';
        _peersEmptyDetail(failed ? 'error' : 'empty');
        return;
    }
    if (!scrollOnly) _setPeersChrome(peers.length, true);

    var filtered = peers;
    if (peersSearch) {
        filtered = filtered.filter(function(c) {
            var name = (c.display_name || '').toLowerCase();
            var hash = (c.hash || '').toLowerCase();
            var statusText = _peerRowProfileStatus(c).toLowerCase();
            return name.indexOf(peersSearch) >= 0 || hash.indexOf(peersSearch) >= 0 || statusText.indexOf(peersSearch) >= 0;
        });
    } else {
        filtered = filtered.slice();
    }

    filtered.sort(function(a, b) {
        if (peersSort === 'name') {
            var aNamed = typeof hasRealDisplayName === 'function' ? hasRealDisplayName(a) : !!a.display_name;
            var bNamed = typeof hasRealDisplayName === 'function' ? hasRealDisplayName(b) : !!b.display_name;
            if (aNamed !== bNamed) return aNamed ? -1 : 1;
            return (a.display_name || a.hash || '').localeCompare(b.display_name || b.hash || '', undefined, { sensitivity: 'base' });
        }
        if (peersSort === 'hops') {
            var ha = a.hops !== null && a.hops !== undefined ? a.hops : 999;
            var hb = b.hops !== null && b.hops !== undefined ? b.hops : 999;
            return (ha - hb) || PeersCache.compareByLastSeen(a, b);
        }
        return PeersCache.compareByLastSeen(a, b);
    });

    if (filtered.length === 0) {
        if (scrollOnly) return;
        container.innerHTML = '<div class="ui-empty"><h3 class="ui-empty-title">No peers match “' + escapeHtml(peersSearch) + '”</h3>' +
            '<p class="ui-empty-body">Search checks names, addresses, and status.</p>' +
            '<div class="ui-empty-actions"><button type="button" class="ui-link" data-peers-clear>Clear search</button></div></div>';
        if (!peersSelectedHash) _peersEmptyDetail('list');
        return;
    }

    // Search opens every group with matches and lifts the cap, so a collapsed group never hides a result.
    var flatItems = RS.buildPeerGroupItems(filtered, peersSearch ? {} : peersCollapsedGroups, function(c) {
        return typeof hasRealDisplayName === 'function' && hasRealDisplayName(c);
    }, { olderDisplayCap: peersSearch ? 0 : RS.PEERS_OLDER_DISPLAY_CAP });

    // Virtualized: only the visible window and a buffer are in the DOM; spacers keep the scroll height.
    var viewportHeight = scrollContainer.clientHeight;
    var metrics = _peerListMetrics(flatItems, measured);
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
    var gap = metrics.gap;

    var startIdx = Math.max(0, _peerIndexAtScrollTop(metrics, _peersScrollTop) - _peersBufferRows);
    startIdx = Math.min(startIdx, flatItems.length);
    var endIdx = Math.min(flatItems.length, _peerIndexAtScrollTop(metrics, _peersScrollTop + viewportHeight) + _peersBufferRows);
    if (endIdx < startIdx) endIdx = startIdx;

    // The caption of the group the window starts in stays in the DOM, so it stays stuck over its rows.
    var pinned = -1;
    if (startIdx > 0 && startIdx < flatItems.length && flatItems[startIdx].type !== 'header') {
        for (var h = startIdx - 1; h >= 0; h--) {
            if (flatItems[h].type === 'header') { pinned = h; break; }
        }
    }

    var unread = _peersUnreadByHash();
    var html = '';
    var top = startIdx > 0 ? metrics.offsets[startIdx] - gap : 0;
    if (pinned >= 0) top -= metrics.heights[pinned] + gap;
    if (top > 0) html += '<div class="peers-spacer" style="height:' + top + 'px"></div>';
    if (pinned >= 0) html += _peerItemHtml(flatItems[pinned], pinned, metrics.heights[pinned], unread, true);
    for (var i = startIdx; i < endIdx; i++) html += _peerItemHtml(flatItems[i], i, metrics.heights[i], unread, false);
    var endOffset = endIdx < flatItems.length ? metrics.offsets[endIdx] : totalHeight + gap;
    var remaining = totalHeight - endOffset;
    if (remaining > 0) html += '<div class="peers-spacer" style="height:' + remaining + 'px"></div>';

    container.innerHTML = html;
    if (!peersSelectedHash) _peersEmptyDetail('list');
}

function _peerItemHtml(item, index, height, unread, pinned) {
    var style = ' data-peer-index="' + index + '" style="height:' + height + 'px"';
    if (item.type === 'header') {
        var open = !!peersSearch || !peersCollapsedGroups[item.group];
        return '<button type="button" class="ui-caption ui-caption--toggle' + (pinned ? ' is-pinned' : '') + '" aria-expanded="' + open + '" data-group="' + item.group + '"' + style + '>' +
            RS.kit.icon('chevron-down') + '<span class="t">' + escapeHtml(item.label) + ' <span class="n">' + item.count.toLocaleString('en-US') + '</span></span></button>';
    }
    if (item.type === 'cap') {
        return '<div class="peers-slot"' + style + '><p class="ui-list-note">' + escapeHtml(item.label) + ' <button type="button" class="ui-link" data-peers-search-focus>Search to find others</button></p></div>';
    }
    return '<div class="peers-slot"' + style + '>' + peerRowHtml(item.data, unread[item.data.hash] || 0) + '</div>';
}

// Compact row: the mark beside the name, one trailing word, and a second line only for a Ratspeak user's status.
function peerRowHtml(c, unread) {
    var named = typeof hasRealDisplayName === 'function' ? hasRealDisplayName(c) : !!c.display_name;
    var status = _peerRowProfileStatus(c);
    var title = named
        ? '<bdi dir="auto">' + escapeHtml(c.display_name) + '</bdi>'
        : '<span class="ui-sr">Unnamed peer, address </span>' + escapeHtml(RS.kit.hash.short(c.hash));
    var tier = RS.kit.presenceFor(c.last_seen);
    var trail = personRowTrail(c, peersSort);
    var mark = named && typeof ratspeakSupportsFeaturesFor === 'function' && ratspeakSupportsFeaturesFor(c) ? RS.kit.mark() : '';
    return '<button type="button" class="ui-row' + (status ? '' : ' ui-row--one') + (named ? '' : ' is-unnamed') + (unread ? ' is-unread' : '') +
            '" data-hash="' + escapeHtml(c.hash) + '"' + (!isCompactLayout() && peersSelectedHash === c.hash ? ' aria-current="true"' : '') + '>' +
        RS.kit.avatar(c.hash, 'compact', tier === 'active' ? 'active' : null, c.last_seen) +
        '<span class="ui-row-main"><span class="ui-row-top ui-row-top--hug"><span class="ui-row-title">' + title + '</span>' + mark +
            '<span class="ui-row-trail">' + escapeHtml(trail) + '</span>' +
            (unread ? '<span class="ui-count" aria-label="' + unread + ' unread">' + unread + '</span>' : '') + '</span>' +
            (status ? '<span class="ui-row-sub"><span class="t" title="' + escapeHtml(status) + '">' + escapeHtml(status) + '</span></span>' : '') +
        '</span></button>';
}

function renderPeersDetailPanel(hash) {
    var content = document.getElementById('peers-detail-content');
    var empty = document.getElementById('peers-detail-empty');
    if (!content) return;
    var model = personModel(hash);
    if (!model.peer) { hidePeersDetail(); return; }

    var html = personDetailHtml(model, 'peer', 'peers-detail');
    if (empty) empty.hidden = true;
    content.hidden = false;
    // The panel re-renders on a timer; leave unchanged markup in place so focus and hover survive.
    if (content._rsLastHtml !== html) {
        content._rsLastHtml = html;
        content.innerHTML = html;
        wirePersonActions(content, model, 'peers-detail', { onBlocked: hidePeersDetail });
    }
}

function hidePeersDetail() {
    var content = document.getElementById('peers-detail-content');
    var empty = document.getElementById('peers-detail-empty');
    if (content) { content.hidden = true; content._rsLastHtml = null; }
    if (empty) empty.hidden = false;
    peersSelectedHash = null;
    _peersLastDirtyKey = '';
    scheduleRenderPeersList();
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
