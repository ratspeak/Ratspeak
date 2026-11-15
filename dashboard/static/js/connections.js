// Default matches the HTML <select>.
var connectionSort = { field: 'name', dir: 'asc' };
var connectionFilter = 'all';
var connectionSearch = '';
var collapsedGroups = { 'online_star': true, 'stale': true, 'offline': true };
var selectedConnection = null;
var _scrollTop = 0;
var _rowHeight = 28;
var _bufferRows = 10;
var _connectionRowMetricKey = '';
var _connectionMetricsBound = false;
var _connectionMetricFrame = null;
var _connectionPendingScrollAnchor = null;

// Lower value = higher sort priority.
var STATUS_ORDER = { direct: 0, reachable: 0, stale: 1, offline: 2, unreachable: 3, unknown: 4 };

function _connsView() {
    return (typeof PeersCache !== 'undefined' && PeersCache) ? PeersCache.enriched() : [];
}

function _measureConnectionRowHeight(scrollContainer) {
    var rootSize = window.getComputedStyle
        ? getComputedStyle(document.documentElement).fontSize
        : '16px';
    var width = Math.max(320, scrollContainer.clientWidth || 0);
    var key = rootSize + '|' + width;
    if (_connectionRowMetricKey === key) return _rowHeight;

    function probe(className, content) {
        var el = document.createElement('div');
        el.className = className;
        el.setAttribute('aria-hidden', 'true');
        el.style.position = 'absolute';
        el.style.visibility = 'hidden';
        el.style.pointerEvents = 'none';
        el.style.width = width + 'px';
        el.style.height = 'auto';
        el.style.inset = '0 auto auto -10000px';
        el.innerHTML = content;
        document.body.appendChild(el);
        var height = Math.ceil(el.getBoundingClientRect().height);
        el.remove();
        return height;
    }

    var rowHeight = probe('conn-row',
        '<span class="conn-status-dot status-reachable"></span>' +
        '<span class="conn-name">Measured peer name</span>' +
        '<span class="conn-row-meta"><span class="conn-hop-badge">2 hops</span>' +
        '<span class="conn-age">just now</span><span class="conn-via">direct</span></span>');
    var headerHeight = probe('conn-group-header',
        '<span class="conn-group-chevron">&#9660;</span>' +
        '<span class="conn-group-label">Recent</span><span class="conn-group-count">(12)</span>');
    _rowHeight = Math.max(28, rowHeight, headerHeight);
    _connectionRowMetricKey = key;
    return _rowHeight;
}

function _resetConnectionRowMetric() {
    if (!_connectionPendingScrollAnchor && _rowHeight > 0) {
        _connectionPendingScrollAnchor = {
            index: Math.floor(_scrollTop / _rowHeight),
            ratio: (_scrollTop % _rowHeight) / _rowHeight
        };
    }
    _connectionRowMetricKey = '';
}

function _invalidateConnectionRowMetric() {
    _resetConnectionRowMetric();
    if (_connectionMetricFrame !== null) return;
    var render = function() {
        _connectionMetricFrame = null;
        renderConnectionsTable(_connsView());
    };
    _connectionMetricFrame = window.requestAnimationFrame
        ? window.requestAnimationFrame(render)
        : setTimeout(render, 16);
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

    if (!_connectionMetricsBound) {
        _connectionMetricsBound = true;
        var relayoutConnections = watchViewLayout('dashboard', _invalidateConnectionRowMetric, function() {
            renderConnectionsTable(_connsView(), true);
        });
        window.addEventListener('ratspeak-text-scale-changed', relayoutConnections);
        if (document.fonts && document.fonts.ready) {
            document.fonts.ready.then(relayoutConnections).catch(function() {});
        }
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

    // Active = recently-heard peer with a real name; Active* = hash-only.
    var flatItems = RS.buildPeerGroupItems(filtered, collapsedGroups, hasRealDisplayName);

    // Mobile uses native scroll; scroll events are no-ops there.
    if (isCompactLayout()) {
        _connectionPendingScrollAnchor = null;
        if (scrollOnly) return;
        var mobileHtml = '';
        flatItems.forEach(function(item) {
            if (item.type === 'header') {
                var isCollapsed = collapsedGroups[item.group];
                var chevron = isCollapsed ? '&#9654;' : '&#9660;';
                mobileHtml += '<div class="conn-group-header" role="button" tabindex="0" aria-expanded="' + (!isCollapsed) + '" data-group="' + item.group + '">' +
                    '<span class="conn-group-chevron">' + chevron + '</span>' +
                    '<span class="conn-group-label">' + item.label + '</span>' +
                    '<span class="conn-group-count">(' + item.count + ')</span>' +
                '</div>';
            } else {
                var c = item.data;
                var isSelected = selectedConnection === c.hash;
                var displayName = c.display_name || (typeof shortHash === 'function' ? shortHash(c.hash, 8, 4) : c.hash);
                var statusClass = 'status-' + c.status;
                var ageText = formatPathAge(c.path_age);
                var ageClass = getAgeColorClass(c.path_age);
                var hopText = c.hops !== null && c.hops !== undefined ? c.hops + (c.hops === 1 ? ' hop' : ' hops') : '\u2014';
                var viaText = formatVia(c.via);
                mobileHtml += '<div class="conn-row' + (isSelected ? ' selected' : '') + '" role="button" tabindex="0" aria-label="Open ' + escapeHtml(displayName) + ', address ' + escapeHtml(c.hash) + '" data-hash="' + escapeHtml(c.hash) + '">' +
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
            if (RS.ui && typeof RS.ui.bindKeyboardActivation === 'function') RS.ui.bindKeyboardActivation(hdr);
        });
        container.querySelectorAll('.conn-row').forEach(function(row) {
            row.addEventListener('click', function() {
                showConnectionDetailSheet(this.dataset.hash);
            });
            if (RS.ui && typeof RS.ui.bindKeyboardActivation === 'function') RS.ui.bindKeyboardActivation(row);
        });
        return;
    }

    _measureConnectionRowHeight(scrollContainer);
    if (_connectionPendingScrollAnchor) {
        _scrollTop = (_connectionPendingScrollAnchor.index + _connectionPendingScrollAnchor.ratio) * _rowHeight;
        scrollContainer.scrollTop = _scrollTop;
        _connectionPendingScrollAnchor = null;
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
            html += '<div class="conn-group-header" role="button" tabindex="0" aria-expanded="' + (!isCollapsed) + '" data-group="' + item.group + '" style="height:' + _rowHeight + 'px">' +
                '<span class="conn-group-chevron">' + chevron + '</span>' +
                '<span class="conn-group-label">' + item.label + '</span>' +
                '<span class="conn-group-count">(' + item.count + ')</span>' +
            '</div>';
        } else {
            var c = item.data;
            var isSelected = selectedConnection === c.hash;
            var displayName = c.display_name || (typeof shortHash === 'function' ? shortHash(c.hash, 8, 4) : c.hash);
            var statusClass = 'status-' + c.status;
            var ageText = formatPathAge(c.path_age);
            var ageClass = getAgeColorClass(c.path_age);
            var hopText = c.hops !== null && c.hops !== undefined ? c.hops + (c.hops === 1 ? ' hop' : ' hops') : '\u2014';
            var viaText = formatVia(c.via);

            html += '<div class="conn-row' + (isSelected ? ' selected' : '') + '" role="button" tabindex="0" aria-label="Open ' + escapeHtml(displayName) + ', address ' + escapeHtml(c.hash) + '" data-hash="' + escapeHtml(c.hash) + '" style="height:' + _rowHeight + 'px">' +
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
        if (RS.ui && typeof RS.ui.bindKeyboardActivation === 'function') RS.ui.bindKeyboardActivation(hdr);
    });

    // Mobile opens the bottom sheet; desktop toggles the inline detail strip.
    container.querySelectorAll('.conn-row').forEach(function(row) {
        row.addEventListener('click', function() {
            var hash = this.dataset.hash;
            if (isCompactLayout()) {
                showConnectionDetailSheet(hash);
            } else {
                selectedConnection = selectedConnection === hash ? null : hash;
                renderConnectionsTable(_connsView());
                renderDetailStrip(hash);
            }
        });
        if (RS.ui && typeof RS.ui.bindKeyboardActivation === 'function') RS.ui.bindKeyboardActivation(row);
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

function showConnectionDetailSheet(hash) {
    var overlay = document.getElementById('conn-detail-sheet-overlay');
    var sheet = document.getElementById('conn-detail-sheet');
    var content = document.getElementById('conn-detail-sheet-content');
    if (!overlay || !sheet || !content) return;

    var contact = null;
    _connsView().forEach(function(c) {
        if (c.hash === hash) contact = c;
    });
    if (!contact) return;

    var name = contact.display_name && contact.display_name !== contact.hash ? contact.display_name : '';
    var saved = typeof _lookupContactRecord === 'function' ? _lookupContactRecord(contact.hash) : null;

    var html = '<div class="conn-detail-sheet-content">' +
        '<div class="conn-detail-sheet-identity">' +
            peerAvatarHtml('conn-detail-sheet-avatar', contact.hash, 64, contact) +
            '<div class="conn-detail-sheet-title">' +
                peerNameHtml('conn-detail-sheet-name', name ? ratspeakDisplayNameHtml(name, contact) : '') +
                '<div class="conn-detail-sheet-hash">' + escapeHtml(contact.hash) + '</div>' +
            '</div>' +
            '<div class="conn-detail-sheet-header-actions">' +
                '<button class="conn-detail-sheet-icon-btn"' + peerCopyAttrs(contact.hash, 'LXMF address', 'Copy LXMF address') + ' title="Copy LXMF address">' +
                    '<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>' +
                '</button>' +
            '</div>' +
        '</div>' +
        '<div class="conn-detail-sheet-actions">' +
            peerActionPairHtml('conn-detail-sheet-primary-actions entity-action-grid', 'conn-sheet', contact.hash) +
        '</div>' +
        peerActionRowsHtml(contact.is_contact) +
        '<div class="conn-detail-sheet-fields">' + peerDetailRowsHtml(contact, { trust: saved ? saved.trust : '' }) + '</div>' +
    '</div>';

    content.innerHTML = html;

    wirePeerActionPair('conn-sheet', contact.hash, closeConnectionDetailSheet);
    wirePeerActionRows(content, contact.hash, { name: name, before: closeConnectionDetailSheet });

    RS.ui.openExistingSheet(sheet, overlay);

    overlay.onclick = function() { closeConnectionDetailSheet(); };
}

function closeConnectionDetailSheet() {
    var overlay = document.getElementById('conn-detail-sheet-overlay');
    var sheet = document.getElementById('conn-detail-sheet');
    if (sheet) RS.ui.closeExistingSheet(sheet, overlay);
}

function openSortSheet() {
    var overlay = document.getElementById('conn-sort-sheet-overlay');
    var sheet = document.getElementById('conn-sort-sheet');
    if (!overlay || !sheet) return;

    var options = sheet.querySelectorAll('.conn-sort-option');
    for (var i = 0; i < options.length; i++) {
        options[i].classList.toggle('active', options[i].getAttribute('data-sort') === connectionSort.field);
    }

    RS.ui.openExistingSheet(sheet, overlay);

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
    if (sheet) RS.ui.closeExistingSheet(sheet, overlay);
}
