// Default matches the HTML <select>.
var connectionSort = { field: 'name', dir: 'asc' };
var connectionFilter = 'all';
var connectionSearch = '';
var collapsedGroups = { 'online_star': true, 'stale': true, 'offline': true };
var selectedConnection = null;
var _scrollTop = 0;
var _rowHeight = 28;
var _bufferRows = 10;

// Lower value = higher sort priority.
var STATUS_ORDER = { direct: 0, reachable: 0, stale: 1, offline: 2, unreachable: 3, unknown: 4 };

function _connsView() {
    return (typeof PeersCache !== 'undefined' && PeersCache) ? PeersCache.enriched() : [];
}

function initConnections() {
    var searchInput = document.getElementById('conn-search');
    if (searchInput) {
        searchInput.addEventListener('input', debounce(function() {
            connectionSearch = this.value.toLowerCase();
            renderConnectionsTable(_connsView());
        }, 150));
    }

    var sortSelect = document.getElementById('conn-sort');
    if (sortSelect) {
        sortSelect.addEventListener('change', function() {
            connectionSort.field = this.value;
            renderConnectionsTable(_connsView());
        });
    }

    var sortBtn = document.getElementById('conn-sort-btn');
    if (sortBtn) {
        sortBtn.addEventListener('click', function() { openSortSheet(); });
    }

    var tableContainer = document.getElementById('conn-table-scroll');
    if (tableContainer) {
        tableContainer.addEventListener('scroll', function() {
            _scrollTop = this.scrollTop;
            renderConnectionsTable(_connsView(), true);
        });
    }

    document.addEventListener('keydown', function(e) {
        var view = document.getElementById('view-dashboard');
        if (!view || !view.classList.contains('active')) return;

        if (e.key === '/' && !e.ctrlKey && !e.metaKey) {
            var search = document.getElementById('conn-search');
            if (search && document.activeElement !== search) {
                e.preventDefault();
                search.focus();
            }
        }
    });

    var bentoSummary = document.getElementById('bento-stats-summary');
    if (bentoSummary) {
        bentoSummary.addEventListener('click', function() {
            this.classList.toggle('expanded');
        });
    }

    // Shows a pulsing placeholder rather than "No connections yet" until real data lands.
    var container = document.getElementById('conn-table-body');
    if (container) {
        container.innerHTML =
            '<div class="empty-state conn-loading-pulse">' +
            '<svg class="empty-state-svg" width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="2" y1="12" x2="22" y2="12"/><path d="M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z"/></svg>' +
            '<span class="empty-state-primary">Loading peers...</span>' +
            '</div>';
    }
}

function updateConnectionBar(data) {
    var pathSummary = typeof pathCountSummary === 'function'
        ? pathCountSummary(data)
        : { visible: (data.path_table || []).length, total: (data.path_table || []).length, truncated: false, label: String((data.path_table || []).length) };
    var reachableCount = 0;
    var view = _connsView();
    view.forEach(function(c) {
        if (c.status === 'reachable' || c.status === 'direct') reachableCount++;
    });
    var totalContacts = view.length;

    var ifaces = data.interface_stats && data.interface_stats.interfaces ? data.interface_stats.interfaces : [];
    var totals = (typeof interfaceStatsTotals === 'function')
        ? interfaceStatsTotals(ifaces)
        : ifaces.reduce(function(acc, i) {
            acc.txb += i.txb || 0;
            acc.rxb += i.rxb || 0;
            return acc;
        }, { txb: 0, rxb: 0 });

    var statsEl = document.getElementById('conn-bar-stats');
    if (statsEl) {
        statsEl.innerHTML =
            '<span class="conn-bar-stat conn-bar-stat-paths" title="' + (pathSummary.truncated ? 'Showing a capped path-table summary' : 'Known paths') + '">' + pathSummary.label + ' <span class="conn-bar-stat-label">paths</span></span>' +
            '<span class="conn-bar-stat">' + reachableCount + ' <span class="conn-bar-stat-label">recent</span></span>' +
            '<span class="conn-bar-stat">' + prettySize(totals.txb) + ' <span class="conn-bar-stat-label">tx</span></span>' +
            '<span class="conn-bar-stat">' + prettySize(totals.rxb) + ' <span class="conn-bar-stat-label">rx</span></span>';
    }

    var bentoPathCount = document.getElementById('bento-path-count');
    if (bentoPathCount) {
        bentoPathCount.textContent = pathSummary.label;
        bentoPathCount.title = pathSummary.truncated ? 'Visible table rows of total known paths' : 'Known paths';
    }

    var bentoOnlineCount = document.getElementById('bento-online-count');
    if (bentoOnlineCount) bentoOnlineCount.textContent = reachableCount + (reachableCount === 1 ? ' peer' : ' peers');

    var bentoTx = document.getElementById('bento-tx');
    if (bentoTx) bentoTx.textContent = prettySize(totalTx);

    var bentoRx = document.getElementById('bento-rx');
    if (bentoRx) bentoRx.textContent = prettySize(totalRx);

    var summaryPeers = document.getElementById('bento-summary-peers');
    if (summaryPeers) summaryPeers.textContent = reachableCount + (reachableCount === 1 ? ' peer' : ' peers');
    var summaryTx = document.getElementById('bento-summary-tx');
    if (summaryTx) summaryTx.textContent = prettySize(totalTx);
    var summaryRx = document.getElementById('bento-summary-rx');
    if (summaryRx) summaryRx.textContent = prettySize(totalRx);

    var bentoDot = document.getElementById('bento-conn-dot');
    var bentoText = document.getElementById('bento-conn-text');
    if (bentoDot && bentoText) {
        if (reachableCount > 0) {
            bentoDot.className = 'dot green';
            bentoText.textContent = 'Recent activity';
        } else {
            bentoDot.className = 'dot orange';
            bentoText.textContent = 'Searching';
        }
    }
}

function renderConnectionsTable(contacts, scrollOnly) {
    if (!scrollOnly && _connectionsThrottleTimer) {
        clearTimeout(_connectionsThrottleTimer);
        _connectionsRenderScheduled = false;
        _connectionsThrottleTimer = null;
    }

    var container = document.getElementById('conn-table-body');
    var scrollContainer = document.getElementById('conn-table-scroll');
    if (!container || !scrollContainer) return;

    if (!contacts || contacts.length === 0) {
        if (!scrollOnly) {
            container.innerHTML =
                '<div class="empty-state">' +
                '<svg class="empty-state-svg" width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="2" y1="12" x2="22" y2="12"/><path d="M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z"/></svg>' +
                '<span class="empty-state-primary">No peers yet</span>' +
                '<span class="empty-state-hint">Connect to a network, device, or add a contact to discover peers</span>' +
                '</div>';
        }
        return;
    }

    var filtered = contacts;
    if (connectionFilter !== 'all') {
        filtered = contacts.filter(function(c) {
            if (connectionFilter === 'reachable') return c.status === 'reachable' || c.status === 'direct';
            if (connectionFilter === 'stale') return c.status === 'stale';
            if (connectionFilter === 'offline') return c.status === 'offline' || c.status === 'unreachable' || c.status === 'unknown';
            return true;
        });
    }

    if (connectionSearch) {
        filtered = filtered.filter(function(c) {
            var name = (c.display_name || '').toLowerCase();
            var hash = (c.hash || '').toLowerCase();
            return name.indexOf(connectionSearch) >= 0 || hash.indexOf(connectionSearch) >= 0;
        });
    }

    filtered.sort(function(a, b) {
        var field = connectionSort.field;
        if (field === 'status') {
            var sa = STATUS_ORDER[a.status] !== undefined ? STATUS_ORDER[a.status] : 9;
            var sb = STATUS_ORDER[b.status] !== undefined ? STATUS_ORDER[b.status] : 9;
            if (sa !== sb) return sa - sb;
            return (a.display_name || a.hash || '').localeCompare(b.display_name || b.hash || '');
        }
        if (field === 'name') {
            return (a.display_name || a.hash || '').localeCompare(b.display_name || b.hash || '');
        }
        if (field === 'hops') {
            var ha = a.hops !== null && a.hops !== undefined ? a.hops : 999;
            var hb = b.hops !== null && b.hops !== undefined ? b.hops : 999;
            return ha - hb;
        }
        if (field === 'last_seen') {
            var ta = a.path_age !== null && a.path_age !== undefined ? a.path_age : 999999;
            var tb = b.path_age !== null && b.path_age !== undefined ? b.path_age : 999999;
            return ta - tb;
        }
        return 0;
    });

    // Recent = recently-heard peer with a real name; Recent* = hash-only.
    var flatItems = RS.buildPeerGroupItems(filtered, collapsedGroups, hasRealDisplayName);

    // Mobile uses native scroll; scroll events are no-ops there.
    if (window.innerWidth <= 768) {
        if (scrollOnly) return;
        var mobileHtml = '';
        flatItems.forEach(function(item) {
            if (item.type === 'header') {
                var isCollapsed = collapsedGroups[item.group];
                var chevron = isCollapsed ? '&#9654;' : '&#9660;';
                mobileHtml += '<div class="conn-group-header" data-group="' + item.group + '">' +
                    '<span class="conn-group-chevron">' + chevron + '</span>' +
                    '<span class="conn-group-label">' + item.label + '</span>' +
                    '<span class="conn-group-count">(' + item.count + ')</span>' +
                '</div>';
            } else {
                var c = item.data;
                var isSelected = selectedConnection === c.hash;
                var displayName = c.display_name || (typeof shortHash === 'function' ? shortHash(c.hash, 8, 4) : c.hash);
                if (displayName.length > 40) displayName = displayName.substring(0, 40) + '\u2026';
                var statusClass = 'status-' + c.status;
                var ageText = formatPathAge(c.path_age);
                var ageClass = getAgeColorClass(c.path_age);
                var hopText = c.hops !== null && c.hops !== undefined ? c.hops + (c.hops === 1 ? ' hop' : ' hops') : '\u2014';
                var viaText = formatVia(c.via);
                mobileHtml += '<div class="conn-row' + (isSelected ? ' selected' : '') + '" data-hash="' + escapeHtml(c.hash) + '">' +
                    '<span class="conn-status-dot ' + statusClass + '"></span>' +
                    '<span class="conn-name">' + ratspeakDisplayNameHtml(displayName, c) + '</span>' +
                    '<span class="conn-row-meta">' +
                        '<span class="conn-hop-badge">' + hopText + '</span>' +
                        '<span class="conn-age ' + ageClass + '">' + ageText + '</span>' +
                        '<span class="conn-via">' + escapeHtml(viaText) + '</span>' +
                    '</span>' +
                '</div>';
            }
        });
        if (container._rsLastHtml === mobileHtml) return;
        container._rsLastHtml = mobileHtml;
        container.innerHTML = mobileHtml;
        container.querySelectorAll('.conn-group-header').forEach(function(hdr) {
            hdr.addEventListener('click', function() {
                var group = this.dataset.group;
                collapsedGroups[group] = !collapsedGroups[group];
                renderConnectionsTable(_connsView());
            });
        });
        container.querySelectorAll('.conn-row').forEach(function(row) {
            row.addEventListener('click', function() {
                showConnectionDetailSheet(this.dataset.hash);
            });
        });
        return;
    }

    var viewportHeight = scrollContainer.clientHeight;
    // Guard: if container isn't laid out yet (view animation), render everything.
    if (viewportHeight < 50) {
        viewportHeight = Math.max(flatItems.length * _rowHeight, 500);
    }
    var totalHeight = flatItems.length * _rowHeight;

    var startIdx = Math.max(0, Math.floor(_scrollTop / _rowHeight) - _bufferRows);
    var endIdx = Math.min(flatItems.length, Math.ceil((_scrollTop + viewportHeight) / _rowHeight) + _bufferRows);

    var html = '';
    if (startIdx > 0) {
        html += '<div style="height:' + (startIdx * _rowHeight) + 'px"></div>';
    }

    for (var i = startIdx; i < endIdx; i++) {
        var item = flatItems[i];
        if (item.type === 'header') {
            var isCollapsed = collapsedGroups[item.group];
            var chevron = isCollapsed ? '&#9654;' : '&#9660;';
            html += '<div class="conn-group-header" data-group="' + item.group + '" style="height:' + _rowHeight + 'px">' +
                '<span class="conn-group-chevron">' + chevron + '</span>' +
                '<span class="conn-group-label">' + item.label + '</span>' +
                '<span class="conn-group-count">(' + item.count + ')</span>' +
            '</div>';
        } else {
            var c = item.data;
            var isSelected = selectedConnection === c.hash;
            var displayName = c.display_name || (typeof shortHash === 'function' ? shortHash(c.hash, 8, 4) : c.hash);
            if (displayName.length > 40) displayName = displayName.substring(0, 40) + '\u2026';
            var statusClass = 'status-' + c.status;
            var ageText = formatPathAge(c.path_age);
            var ageClass = getAgeColorClass(c.path_age);
            var hopText = c.hops !== null && c.hops !== undefined ? c.hops + (c.hops === 1 ? ' hop' : ' hops') : '\u2014';
            var viaText = formatVia(c.via);

            html += '<div class="conn-row' + (isSelected ? ' selected' : '') + '" data-hash="' + escapeHtml(c.hash) + '" style="height:' + _rowHeight + 'px">' +
                '<span class="conn-status-dot ' + statusClass + '"></span>' +
                '<span class="conn-name">' + ratspeakDisplayNameHtml(displayName, c) + '</span>' +
                '<span class="conn-row-meta">' +
                    '<span class="conn-hop-badge">' + hopText + '</span>' +
                    '<span class="conn-age ' + ageClass + '">' + ageText + '</span>' +
                    '<span class="conn-via">' + escapeHtml(viaText) + '</span>' +
                '</span>' +
            '</div>';
        }
    }

    var renderedHeight = (endIdx - startIdx) * _rowHeight;
    var bottomSpace = totalHeight - (startIdx * _rowHeight) - renderedHeight;
    if (bottomSpace > 0) {
        html += '<div style="height:' + bottomSpace + 'px"></div>';
    }

    container.innerHTML = html;

    container.querySelectorAll('.conn-group-header').forEach(function(hdr) {
        hdr.addEventListener('click', function() {
            var group = this.dataset.group;
            collapsedGroups[group] = !collapsedGroups[group];
            renderConnectionsTable(_connsView());
        });
    });

    // Mobile opens the bottom sheet; desktop toggles the inline detail strip.
    container.querySelectorAll('.conn-row').forEach(function(row) {
        row.addEventListener('click', function() {
            var hash = this.dataset.hash;
            if (window.innerWidth <= 768) {
                showConnectionDetailSheet(hash);
            } else {
                selectedConnection = selectedConnection === hash ? null : hash;
                renderConnectionsTable(_connsView());
                renderDetailStrip(hash);
            }
        });
    });

}

function renderDetailStrip(hash) {
    var strip = document.getElementById('conn-detail-strip');
    if (!strip) return;

    if (!hash || !selectedConnection) {
        strip.style.display = 'none';
        return;
    }

    var contact = null;
    _connsView().forEach(function(c) {
        if (c.hash === hash) contact = c;
    });

    if (!contact) {
        strip.style.display = 'none';
        return;
    }

    strip.style.display = 'flex';
    var ageText = contact.path_age !== null && contact.path_age !== undefined
        ? prettyTime(contact.path_age) + ' ago' : 'No path';
    var lastHeardText = typeof formatLastHeard === 'function' ? formatLastHeard(contact.last_seen) : (contact.last_seen ? new Date(contact.last_seen * 1000).toLocaleString() : 'No activity yet');
    var nameText = contact.display_name || contact.hash;

    strip.innerHTML =
        '<div class="detail-actions">' +
            (contact.is_contact ? '' : '<button class="conn-add-btn" data-hash="' + escapeHtml(contact.hash) + '">Add</button>') +
            '<button class="conn-msg-btn" data-hash="' + escapeHtml(contact.hash) + '">Message</button>' +
        '</div>' +
        '<div class="detail-sep"></div>' +
        '<div class="detail-field detail-field-name"><span class="detail-label">Name</span><span class="detail-value" title="' + escapeHtml(nameText) + '">' + ratspeakDisplayNameHtml(nameText, contact) + '</span></div>' +
        '<div class="detail-field detail-field-hash"><span class="detail-label">Hash</span><span class="detail-value mono">' + copyableHash(contact.hash) + '</span></div>' +
        '<div class="detail-field"><span class="detail-label">Last heard</span><span class="detail-value">' + escapeHtml(lastHeardText) + '</span></div>' +
        '<div class="detail-field"><span class="detail-label">Route</span><span class="detail-value">' + escapeHtml(contact.route_label || 'No current path') + '</span></div>' +
        '<div class="detail-field"><span class="detail-label">Path Age</span><span class="detail-value">' + ageText + '</span></div>' +
        (contact.via ? '<div class="detail-field"><span class="detail-label">Via</span><span class="detail-value mono">' + escapeHtml(typeof shortHash === 'function' ? shortHash(contact.via, 6, 4) : (contact.via.length > 6 ? contact.via.substring(0, 6) + '\u2026' : contact.via)) + '</span></div>' : '') +
        (contact.hops !== null && contact.hops !== undefined ? '<div class="detail-field"><span class="detail-label">Hops</span><span class="detail-value">' + contact.hops + '</span></div>' : '');

    strip.querySelectorAll('.conn-msg-btn').forEach(function(btn) {
        btn.addEventListener('click', function(e) {
            e.stopPropagation();
            var h = this.dataset.hash;
            if (typeof openConversationWith === 'function') openConversationWith(h);
        });
    });

    strip.querySelectorAll('.conn-add-btn').forEach(function(btn) {
        btn.addEventListener('click', function(e) {
            e.stopPropagation();
            var h = this.dataset.hash;
            var contactData = null;
            _connsView().forEach(function(c) { if (c.hash === h) contactData = c; });
            var prefillName = contactData ? (contactData.display_name || '') : '';
            rsPrompt({ message: 'Contact name (optional):', placeholder: 'Display name', defaultValue: prefillName }).then(function(name) {
                if (name === null) return;
                RS.invoke('add_contact', { args: { hash: h, display_name: name.trim() || null } }).catch(function() {});
            });
        });
    });

    _setupDetailObserver();
    requestAnimationFrame(_updateHashTruncation);
}

var _detailObserver = null;
var _monoCharWidth = null;

function _getMonoCharWidth() {
    if (_monoCharWidth !== null) return _monoCharWidth;
    var span = document.createElement('span');
    span.className = 'mono-measure';
    span.textContent = 'aaaaaaaaaa';
    document.body.appendChild(span);
    _monoCharWidth = span.offsetWidth / 10;
    document.body.removeChild(span);
    return _monoCharWidth || 6;
}

function _updateHashTruncation() {
    var strip = document.getElementById('conn-detail-strip');
    if (!strip || strip.style.display === 'none') return;

    var hashCopy = strip.querySelector('.detail-field-hash .hash-copy');
    if (!hashCopy) return;

    var fullHash = hashCopy.getAttribute('data-full');
    if (!fullHash || fullHash.length <= 7) return;

    var fieldEl = hashCopy.closest('.detail-field-hash');
    if (!fieldEl) return;
    var availWidth = fieldEl.clientWidth;
    if (availWidth <= 0) return;

    var charWidth = _getMonoCharWidth();
    var maxChars = Math.floor(availWidth / charWidth);

    if (maxChars >= fullHash.length) {
        hashCopy.textContent = fullHash;
    } else if (maxChars >= 7) {
        var front = maxChars - 4;
        hashCopy.textContent = fullHash.substring(0, front) + '\u2026' + fullHash.slice(-3);
    } else {
        hashCopy.textContent = fullHash.substring(0, 3) + '\u2026' + fullHash.slice(-3);
    }
}

function _setupDetailObserver() {
    var strip = document.getElementById('conn-detail-strip');
    if (!strip) return;
    if (_detailObserver) _detailObserver.disconnect();
    if (typeof ResizeObserver !== 'undefined') {
        _detailObserver = new ResizeObserver(function() {
            _updateHashTruncation();
        });
        _detailObserver.observe(strip);
    }
}

function formatPathAge(seconds) {
    if (seconds === null || seconds === undefined) return '\u2014';
    if (seconds < 60) return Math.floor(seconds) + 's';
    if (seconds < 3600) return Math.floor(seconds / 60) + 'm';
    if (seconds < 86400) return Math.floor(seconds / 3600) + 'h';
    return Math.floor(seconds / 86400) + 'd';
}

function getAgeColorClass(seconds) {
    if (seconds === null || seconds === undefined) return 'age-none';
    if (seconds < 1800) return 'age-fresh';
    if (seconds < 3600) return 'age-stale';
    return 'age-old';
}

function formatVia(via) {
    if (!via) return 'direct';
    if (via.length > 12) return 'via ' + via.substring(0, 8) + '\u2026';
    return 'via ' + via;
}

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

function updateMessageReachability() {
    var view = _connsView();
    if (view.length === 0) return;

    var lookup = {};
    view.forEach(function(c) {
        lookup[c.hash] = c;
    });

    document.querySelectorAll('.lxmf-contact .contact-id-status').forEach(function(dot) {
        var row = dot.closest('.lxmf-contact');
        if (!row) return;
        var hash = row.dataset.hash;
        var info = lookup[hash];
        if (info) {
            dot.className = 'contact-id-status status-' + info.status;
            dot.title = (info.activity_label || 'Never seen') + ' - ' + (info.route_label || 'No current path');
        }
    });
}

function refreshConnectionsTable() {
    if (_connectionsThrottleTimer) clearTimeout(_connectionsThrottleTimer);
    _connectionsRenderScheduled = false;
    _connectionsThrottleTimer = null;
    var view = _connsView();
    if (view.length > 0) {
        renderConnectionsTable(view);
    }
    // _ptrAttached guards make this idempotent across re-renders.
    var connScroll = document.getElementById('conn-table-scroll');
    if (connScroll && !connScroll._ptrAttached) {
        RS.gestures.attachPullToRefresh(connScroll, { onRefresh: refreshConnectionsTable });
    }
}

function showConnectionDetailSheet(hash, options) {
    options = options || {};
    var overlay = document.getElementById('conn-detail-sheet-overlay');
    var sheet = document.getElementById('conn-detail-sheet');
    var content = document.getElementById('conn-detail-sheet-content');
    if (!overlay || !sheet || !content) return;

    var contact = null;
    _connsView().forEach(function(c) {
        if (c.hash === hash) contact = c;
    });
    if (!contact) return;

    var nameText = contact.display_name || (typeof shortHash === 'function' ? shortHash(contact.hash, 8, 4) : contact.hash);
    var hashText = escapeHtml(contact.hash);
    var avatarHtml = (typeof identityAvatar === 'function') ? identityAvatar(contact.hash, 64) : '';
    var lastHeardText = typeof formatLastHeard === 'function' ? formatLastHeard(contact.last_seen) : (contact.last_seen ? new Date(contact.last_seen * 1000).toLocaleString() : 'No activity yet');
    var firstHeardText = contact.first_seen ? (typeof formatLastHeard === 'function' ? formatLastHeard(contact.first_seen) : new Date(contact.first_seen * 1000).toLocaleString()) : '\u2014';
    function routeDetailText(label, pathAge, iface) {
        var text = label || 'No current path';
        var meta = [];
        if (pathAge !== null && pathAge !== undefined) meta.push(prettyTime(pathAge) + ' ago');
        if (iface) meta.push(iface);
        return meta.length ? text + ' · ' + meta.join(' · ') : text;
    }
    var messageRouteText = routeDetailText(
        contact.message_route_label || 'No current path',
        contact.message_path_age,
        contact.message_iface
    );
    var callRouteText = routeDetailText(
        contact.voice_route_label || 'No current path',
        contact.voice_path_age,
        contact.voice_iface
    );
    if (!contact.telephony_hash && !contact.supports_lxst_call && !contact.voice_in_path) {
        callRouteText = 'Not announced';
    }
    function summarizeRouteAges() {
        var parts = [];
        if (contact.message_path_age !== null && contact.message_path_age !== undefined) {
            parts.push('Message: ' + prettyTime(contact.message_path_age) + ' ago');
        }
        if (contact.voice_path_age !== null && contact.voice_path_age !== undefined) {
            parts.push('Call: ' + prettyTime(contact.voice_path_age) + ' ago');
        }
        return parts.length ? parts.join(' · ') : '\u2014';
    }
    function summarizeRouteIfaces() {
        var parts = [];
        if (contact.message_iface) parts.push('Message: ' + contact.message_iface);
        if (contact.voice_iface && contact.voice_iface !== contact.message_iface) {
            parts.push('Call: ' + contact.voice_iface);
        }
        if (parts.length) return parts.join(' · ');
        return contact.iface ? contact.iface + (contact.iface_is_live ? '' : ' (last known)') : '\u2014';
    }
    var pathAgeText = summarizeRouteAges();
    // TODO(dev-mode): expose next-hop/via once developer-mode detail rows exist.
    var ifaceText = summarizeRouteIfaces();
    var progressive = !!options.progressive;
    var callActionHtml = typeof voiceActionButtonHtml === 'function' ? voiceActionButtonHtml('conn-sheet-call-btn', contact.hash) : '';
    var addActionHtml = !contact.is_contact
        ? '<button class="nr-btn entity-action-btn conn-sheet-secondary-action" id="conn-sheet-add-btn" data-hash="' + escapeHtml(contact.hash) + '"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M16 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2"/><circle cx="8.5" cy="7" r="4"/><line x1="20" y1="8" x2="20" y2="14"/><line x1="23" y1="11" x2="17" y2="11"/></svg><span>Add</span></button>'
        : '';
    var messageActionHtml = '<button class="nr-btn entity-action-btn conn-sheet-message-action" id="conn-sheet-msg-btn" data-hash="' + escapeHtml(contact.hash) + '"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/></svg><span>Message</span></button>';
    var secondaryActionsHtml = addActionHtml
        ? '<div class="conn-detail-sheet-secondary-actions entity-action-grid">' + addActionHtml + '</div>'
        : '';

    var html = '<div class="conn-detail-sheet-content">' +
        '<div class="conn-detail-sheet-identity">' +
            '<div class="conn-detail-sheet-avatar">' + avatarHtml + '</div>' +
            '<div class="conn-detail-sheet-title">' +
                '<div class="conn-detail-sheet-name">' + ratspeakDisplayNameHtml(nameText, contact) + '</div>' +
                '<div class="conn-detail-sheet-hash">' + hashText + '</div>' +
            '</div>' +
            '<div class="conn-detail-sheet-header-actions">' +
                '<button type="button" class="conn-detail-sheet-icon-btn" id="conn-sheet-copy-btn" data-hash="' + escapeHtml(contact.hash) + '" title="Copy LXMF address" aria-label="Copy LXMF address">' +
                    '<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>' +
                '</button>' +
                '<button type="button" class="conn-detail-sheet-icon-btn" id="conn-sheet-more-btn" data-hash="' + escapeHtml(contact.hash) + '" title="More actions" aria-label="More actions" aria-haspopup="menu">' +
                    '<svg viewBox="0 0 24 24" width="16" height="16" fill="currentColor" aria-hidden="true"><circle cx="12" cy="5" r="2"/><circle cx="12" cy="12" r="2"/><circle cx="12" cy="19" r="2"/></svg>' +
                '</button>' +
            '</div>' +
        '</div>' +
        '<div class="conn-detail-sheet-actions">' +
            '<div class="conn-detail-sheet-primary-actions entity-action-grid">' + callActionHtml + messageActionHtml + '</div>' +
            secondaryActionsHtml +
        '</div>' +
        (progressive
            ? '<button type="button" class="conn-detail-sheet-expand-hint" id="conn-sheet-expand-hint" aria-label="Swipe up for more info">' +
                '<span>Swipe up for more info</span>' +
                '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M6 15l6-6 6 6"/></svg>' +
            '</button>'
            : '') +
        '<div class="conn-detail-sheet-fields">' +
            '<div class="conn-detail-sheet-field"><span>Last heard</span><strong>' + escapeHtml(lastHeardText) + '</strong></div>' +
            '<div class="conn-detail-sheet-field"><span>First heard</span><strong>' + escapeHtml(firstHeardText) + '</strong></div>' +
            '<div class="conn-detail-sheet-field"><span>Message route</span><strong>' + escapeHtml(messageRouteText) + '</strong></div>' +
            '<div class="conn-detail-sheet-field"><span>Call route</span><strong>' + escapeHtml(callRouteText) + '</strong></div>' +
            '<div class="conn-detail-sheet-field"><span>Path age</span><strong>' + escapeHtml(pathAgeText) + '</strong></div>' +
            '<div class="conn-detail-sheet-field"><span>Interface</span><strong>' + escapeHtml(ifaceText) + '</strong></div>' +
        '</div>' +
    '</div>';

    content.innerHTML = html;
    sheet.classList.toggle('conn-detail-sheet--progressive', progressive);
    sheet.classList.toggle('conn-detail-sheet--compact', progressive && !addActionHtml);
    sheet.classList.toggle('conn-detail-sheet--with-add', progressive && !!addActionHtml);
    sheet.classList.remove('conn-detail-sheet--expanded');
    sheet._progressiveExpansionEnabled = progressive;

    var copyBtn = document.getElementById('conn-sheet-copy-btn');
    if (copyBtn) {
        copyBtn.addEventListener('click', function(ev) {
            ev.stopPropagation();
            var h = this.dataset.hash;
            if (navigator.clipboard) {
                navigator.clipboard.writeText(h);
                if (typeof showCopyConfirmationToast === 'function') showCopyConfirmationToast('Address');
            }
        });
    }

    var addBtn = document.getElementById('conn-sheet-add-btn');
    if (addBtn) {
        addBtn.addEventListener('click', function() {
            var h = this.dataset.hash;
            var contactData = null;
            _connsView().forEach(function(c) { if (c.hash === h) contactData = c; });
            var prefillName = contactData ? (contactData.display_name || '') : '';
            closeConnectionDetailSheet();
            rsPrompt({ message: 'Contact name (optional):', placeholder: 'Display name', defaultValue: prefillName }).then(function(name) {
                if (name === null) return;
                RS.invoke('add_contact', { args: { hash: h, display_name: name.trim() || null } }).catch(function() {});
            });
        });
    }

    var msgBtn = document.getElementById('conn-sheet-msg-btn');
    if (msgBtn) {
        msgBtn.addEventListener('click', function() {
            var h = this.dataset.hash;
            closeConnectionDetailSheet();
            if (typeof openConversationWith === 'function') openConversationWith(h);
        });
    }
    if (typeof wireVoiceActionButton === 'function') {
        wireVoiceActionButton('conn-sheet-call-btn', closeConnectionDetailSheet);
    }

    var moreBtn = document.getElementById('conn-sheet-more-btn');
    if (moreBtn) {
        moreBtn.addEventListener('click', function(ev) {
            ev.stopPropagation();
            var h = this.dataset.hash;
            if (typeof actionPopover !== 'function') {
                confirmBlockPeer(h);
                return;
            }
            actionPopover(this, [{
                label: 'Block',
                danger: true,
                icon: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="4.93" y1="4.93" x2="19.07" y2="19.07"/></svg>',
                onSelect: function() { confirmBlockPeer(h); }
            }]);
        });
    }

    function confirmBlockPeer(h) {
        var contactData = null;
        _connsView().forEach(function(c) { if (c.hash === h) contactData = c; });
        var displayName = contactData ? (contactData.display_name || (typeof shortHash === 'function' ? shortHash(h, 8, 4) : h.substring(0, 12))) : (typeof shortHash === 'function' ? shortHash(h, 8, 4) : h.substring(0, 12));
        closeConnectionDetailSheet();
        rsConfirmWithCheckbox({
            message: 'Block "' + displayName + '"? They won\'t appear in your peers list and their messages will be hidden.',
            danger: true,
            confirmText: 'Block',
            checkboxLabel: 'Also block at the network layer (drop their packets entirely)',
            checkboxHelp: 'Stops their messages from reaching this device at all, instead of just hiding them. Useful for relay nodes. This affects all accounts on this device.',
            defaultChecked: false
        }).then(function(result) {
            if (!result.confirmed) return;
            RS.invoke('block_contact', { args: { hash: h, escalate_to_blackhole: result.checked } })
                .then(function(resp) {
                    if (resp && resp.blackhole_pending && typeof showToast === 'function') {
                        showToast('Blocked. Network blackhole will activate on their next announce.', 'toast-orange', 5000);
                    }
                })
                .catch(function() {});
        });
    }

    var expandHint = document.getElementById('conn-sheet-expand-hint');
    if (expandHint) {
        expandHint.addEventListener('click', expandConnectionDetailSheet);
    }
    wireConnectionDetailExpansion(sheet);

    overlay.classList.add('active');
    sheet.classList.add('open');

    overlay.onclick = function() { closeConnectionDetailSheet(); };

    sheet._escHandler = function(e) {
        if (e.key === 'Escape') closeConnectionDetailSheet();
    };
    document.addEventListener('keydown', sheet._escHandler);
}

function expandConnectionDetailSheet() {
    var sheet = document.getElementById('conn-detail-sheet');
    if (!sheet || !sheet.classList.contains('conn-detail-sheet--progressive')) return;
    sheet.classList.add('conn-detail-sheet--expanded');
    var content = document.getElementById('conn-detail-sheet-content');
    if (content) {
        requestAnimationFrame(function() {
            var fields = content.querySelector('.conn-detail-sheet-fields');
            if (fields && typeof fields.scrollIntoView === 'function') {
                fields.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
            }
        });
    }
}

function wireConnectionDetailExpansion(sheet) {
    if (!sheet || sheet._progressiveExpandWired) return;
    sheet._progressiveExpandWired = true;
    sheet._progressiveStartY = null;

    sheet.addEventListener('touchstart', function(e) {
        if (!sheet._progressiveExpansionEnabled || sheet.classList.contains('conn-detail-sheet--expanded')) return;
        if (!e.touches || e.touches.length !== 1) return;
        sheet._progressiveStartY = e.touches[0].clientY;
    }, { passive: true });

    sheet.addEventListener('touchend', function(e) {
        if (!sheet._progressiveExpansionEnabled || sheet.classList.contains('conn-detail-sheet--expanded')) return;
        if (sheet._progressiveStartY === null || !e.changedTouches || e.changedTouches.length < 1) return;
        var dy = e.changedTouches[0].clientY - sheet._progressiveStartY;
        sheet._progressiveStartY = null;
        if (dy < -28) expandConnectionDetailSheet();
    }, { passive: true });
}

function closeConnectionDetailSheet() {
    var overlay = document.getElementById('conn-detail-sheet-overlay');
    var sheet = document.getElementById('conn-detail-sheet');
    if (typeof closeActionPopover === 'function') closeActionPopover();
    if (overlay) overlay.classList.remove('active');
    if (sheet) {
        sheet.classList.remove('open');
        sheet.classList.remove('conn-detail-sheet--progressive', 'conn-detail-sheet--expanded', 'conn-detail-sheet--compact', 'conn-detail-sheet--with-add');
        sheet._progressiveExpansionEnabled = false;
        if (sheet._escHandler) {
            document.removeEventListener('keydown', sheet._escHandler);
            sheet._escHandler = null;
        }
    }
}

function openSortSheet() {
    var overlay = document.getElementById('conn-sort-sheet-overlay');
    var sheet = document.getElementById('conn-sort-sheet');
    if (!overlay || !sheet) return;

    var options = sheet.querySelectorAll('.conn-sort-option');
    for (var i = 0; i < options.length; i++) {
        options[i].classList.toggle('active', options[i].getAttribute('data-sort') === connectionSort.field);
    }

    overlay.classList.add('active');
    sheet.classList.add('open');

    for (var j = 0; j < options.length; j++) {
        options[j].onclick = function() {
            connectionSort.field = this.getAttribute('data-sort');
            var sel = document.getElementById('conn-sort');
            if (sel) sel.value = connectionSort.field;
            renderConnectionsTable(_connsView());
            closeSortSheet();
        };
    }

    overlay.onclick = function() { closeSortSheet(); };
}

function closeSortSheet() {
    var overlay = document.getElementById('conn-sort-sheet-overlay');
    var sheet = document.getElementById('conn-sort-sheet');
    if (overlay) overlay.classList.remove('active');
    if (sheet) sheet.classList.remove('open');
}
