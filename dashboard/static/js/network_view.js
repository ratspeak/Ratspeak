// Network: the map and Connectivity. One requestAnimationFrame loop draws the map's
// moving dots, and only while the view and page are visible and something moves.
(function() {
    'use strict';
    var MIN = 60000;
    var KIND = {
        tcp: { label: 'Internet', icon: 'globe', tile: 'info' },
        ble: { label: 'Bluetooth Peer', icon: 'bluetooth', tile: 'purple' },
        local: { label: 'Local Network', icon: 'wifi', tile: 'warning' },
        lora: { label: 'Radio', icon: 'tower', tile: 'online' },
        host: { label: 'Host', icon: 'server', tile: 'neutral' },
        discovered: { label: 'Discovered', icon: 'discover', tile: 'neutral' }
    };
    var DURS = [[600, '10 min'], [1800, '30 min'], [3600, '1 hour'], [0, 'Always']];
    var BLE_DEFAULT_SECS = 1800;

    var NV = RS.networkView = {
        sel: null, view: null, dfilter: 'all', older: false, moreOpen: false, joinOpen: false,
        mapCollapsed: false, announcing: false, lastAnnounce: 0, hist: [], lastTotal: null,
        bleStarting: false, setupFor: null, ribbonWas: null,
        internetOpen: false, waysCollapsed: [], connectedOnce: false, flash: null
    };

    function K() { return RS.kit; }
    function icon(name, cls) { return K() ? K().icon(name, cls) : ''; }
    // Folded points right, open points down, like the app's other folds.
    function disclosure(open) { return icon(open ? 'chevron-down' : 'chevron-right'); }
    function esc(v) { return escapeHtml(v == null ? '' : String(v)); }
    function compact() { return typeof isCompactLayout === 'function' && isCompactLayout(); }
    function dot(tone) { return '<span class="ui-dot' + (tone ? ' ui-dot--' + tone : '') + '" aria-hidden="true"></span>'; }
    function btn(label, o) {
        o = o || {};
        return '<button type="button" class="ui-btn' + (o.cls ? ' ' + o.cls : '') + '"' + (o.act ? ' data-act="' + o.act + '"' : '') + (o.attrs || '') + (o.disabled ? ' disabled' : '') + '>' +
            (o.spin ? '<span class="ui-spin" aria-hidden="true"></span>' : '') + (o.icon ? icon(o.icon) : '') +
            (o.iconOnly ? '<span class="ui-sr">' + esc(label) + '</span>' : esc(label)) + '</button>';
    }
    function link(label, act, attrs, ic) {
        return '<button type="button" class="ui-link" data-act="' + act + '"' + (attrs || '') + '>' + (ic ? icon(ic) : '') + esc(label) + '</button>';
    }
    function sw(o) {
        return '<label class="ui-switch' + (o.busy ? ' is-busy' : '') + '"><input type="checkbox" role="switch" data-act="' + o.act + '"' +
            (o.on ? ' checked' : '') + (o.busy ? ' disabled' : '') + ' aria-label="' + esc(o.label) + '"><span class="track"></span></label>';
    }
    function seg(items, cur, label, act) {
        return '<div class="ui-seg ui-seg--block" role="radiogroup" aria-label="' + esc(label) + '">' + items.map(function(x) {
            return '<button type="button" role="radio" aria-checked="' + (String(cur) === String(x[0])) + '" data-act="' + act + '" data-v="' + esc(x[0]) + '">' + esc(x[1]) + '</button>';
        }).join('') + '</div>';
    }

    // Totals never ellipsize: 3 significant digits at most, plus the unit.
    function bytes(n) {
        n = Number(n) || 0;
        if (n < 999.5) return Math.round(n) + ' B';
        var u = ['KB', 'MB', 'GB', 'TB'], i = -1;
        do { n /= 1024; i++; } while (n >= 999.5 && i < u.length - 1);
        return (n < 9.95 ? n.toFixed(1) : String(Math.round(n))) + ' ' + u[i];
    }
    var BYTES_WIDEST = ['999 B', '9.9 KB', '999 KB', '9.9 MB', '999 MB', '9.9 GB', '999 GB', '999 TB'];
    var AGO_WIDEST = ['just now', '59 min ago', '23 h ago', '99 d ago'];
    function ago(ms) {
        var m = Math.floor(ms / MIN);
        if (m < 1) return 'just now';
        if (m < 60) return m + ' min ago';
        var h = Math.floor(m / 60);
        return h < 24 ? h + ' h ago' : Math.floor(h / 24) + ' d ago';
    }
    function dur(ms) {
        var m = Math.max(1, Math.round(ms / MIN));
        if (m < 60) return m + ' min';
        var h = Math.floor(m / 60), r = m % 60;
        return r ? h + ' h ' + r + ' min' : h + (h === 1 ? ' hour' : ' hours');
    }
    function spark(hist, w, h) {
        var max = Math.max.apply(null, hist.concat([1])), n = Math.max(2, hist.length);
        var pts = hist.map(function(v, i) { return (i * w / (n - 1)).toFixed(1) + ',' + (h - 2 - (v / max) * (h - 4)).toFixed(1); });
        if (pts.length < 2) pts = ['0,' + (h - 2), w + ',' + (h - 2)];
        return '<svg class="net-spark" viewBox="0 0 ' + w + ' ' + h + '" preserveAspectRatio="none" aria-hidden="true">' +
            '<polygon class="net-spark-fill" points="0,' + h + ' ' + pts.join(' ') + ' ' + w + ',' + h + '"/>' +
            '<polyline class="net-spark-line" points="' + pts.join(' ') + '"/></svg>';
    }

    // ------------------------------------------------------------------ the model, from the app's caches
    function lifecycle() {
        var ifaces = window._cachedConfigIfaces || {};
        return ifaces.lifecycle || { paused_all: false, paused_names: [], ble_peer_paused: false, paused_count: 0, off: [] };
    }
    function publicServerFor(iface) {
        if (typeof PUBLIC_TCP_SERVERS === 'undefined' || typeof _publicServerMatchesEndpoint !== 'function') return null;
        for (var i = 0; i < PUBLIC_TCP_SERVERS.length; i++) {
            if (_publicServerMatchesEndpoint(PUBLIC_TCP_SERVERS[i], iface.target_host, iface.target_port)) return PUBLIC_TCP_SERVERS[i];
        }
        return null;
    }

    function model() {
        var life = lifecycle();
        var off = life.off || [];
        var items = typeof networkConnectionItems === 'function' ? networkConnectionItems() : [];
        var conns = items.map(function(a) {
            var iface = a.iface || {};
            var name = iface.name || 'unknown';
            var live = typeof getInterfaceLiveStatus === 'function' ? getInterfaceLiveStatus(name) : null;
            var online = live ? live.online !== false : false;
            var status = a.paused ? (off.indexOf(name) !== -1 ? 'off' : 'paused') : (online ? 'up' : 'pending');
            var display = typeof friendlyInterfaceName === 'function' ? friendlyInterfaceName(name, iface.type || '', iface.role || 'normal') : name;
            // iOS without the multicast entitlement: Local Network can't hear anyone.
            var au = a.section === 'local' && !a.paused ? window._autoUnavailable : null;
            if (au && au.interface === name) status = 'pending';
            var trouble = status === 'pending' && typeof connectTroubleText === 'function' ? connectTroubleText(live) : '';
            return {
                id: name, name: display, kind: a.section, ifaceType: a.ifaceType, iface: iface, status: status,
                tx: live ? live.txb || 0 : 0, rx: live ? live.rxb || 0 : 0,
                peers: live && live.peers != null ? live.peers : null, clients: live && live.clients != null ? live.clients : null,
                pub: a.section === 'tcp' ? publicServerFor(iface) : null, joined: a.ifaceType === 'joined',
                health: au && au.interface === name ? 'Multicast unavailable' : a.mobileHealth ? a.mobileHealth.label : (a.waitingForDevice ? 'Waiting for USB' : trouble)
            };
        }).filter(function(c) { return c.kind !== 'ble'; });
        var bleOn = !!window._blePeerEnabled || !!window._blePeerPaused;
        if (bleOn) {
            var peers = typeof _bleVisiblePeersFromCache === 'function' ? _bleVisiblePeersFromCache() : [];
            var paused = !!window._blePeerPaused;
            if (!peers.length) conns.push({ id: 'ble:scan', name: 'Nobody found yet', kind: 'ble', scanning: !paused, status: paused ? 'paused' : 'up', tx: 0, rx: 0 });
            peers.forEach(function(p) {
                conns.push({ id: 'ble:' + (p.identity_hash || p.address), name: typeof _resolveBlePeerLabel === 'function' ? _resolveBlePeerLabel(p).label : (p.address || 'Peer'),
                    kind: 'ble', status: paused ? 'paused' : 'up', tx: 0, rx: 0, peer: p, since: p.connected_at || 0 });
            });
        }
        var stats = typeof lastStats !== 'undefined' && lastStats ? lastStats : null;
        var totals = stats && stats.interface_stats && typeof interfaceStatsTotals === 'function'
            ? interfaceStatsTotals(stats.interface_stats.interfaces || []) : { txb: 0, rxb: 0 };
        var ns = RS.networkSettings || {};
        var settings = Object.assign({ discover: false, autojoin: 0, trusted_publishers: [] }, ns.settings || {});
        return {
            phone: compact(), conns: conns, life: life, pausedAll: !!life.paused_all,
            ble: { on: bleOn, starting: NV.bleStarting, duration: window._blePeerDuration, expiresAt: window._blePeerExpiresAt || 0,
                state: typeof _deriveBlePeerState === 'function' ? _deriveBlePeerState() : '' },
            disc: { on: !!settings.discover, settings: settings, entries: ((ns.discovered || {}).entries || []) },
            tx: totals.txb || 0, rx: totals.rxb || 0,
            activePeers: typeof PeersCache !== 'undefined' && PeersCache && typeof PeersCache.enriched === 'function' ? PeersCache.enriched().filter(RS.peerIsActive).length : 0,
            lastAnnounce: Math.max(NV.lastAnnounce || 0, stats && stats.last_announce_at ? stats.last_announce_at * 1000 : 0),
            me: activeMe()
        };
    }
    function activeMe() {
        var a = typeof activeIdentity === 'function' ? activeIdentity() : null;
        return { name: a ? (a.display_name || a.nickname || 'You') : 'You', hash: a ? (a.lxmf_hash || a.hash || '') : '' };
    }
    function byKind(M, k) { return M.conns.filter(function(c) { return c.kind === k; }); }
    function byId(M, id) { return M.conns.filter(function(c) { return c.id === id; })[0]; }
    function live(M) { return M.conns.filter(function(c) { return c.status === 'up' && !c.scanning; }); }
    function allPaused(lst) { return lst.length > 0 && lst.every(function(c) { return c.status === 'paused'; }); }
    function bleLeft(M) {
        if (!M.ble.expiresAt) return 'Always on';
        var m = Math.max(1, Math.ceil((M.ble.expiresAt * 1000 - Date.now()) / MIN));
        if (m < 60) return m + ' min left';
        var h = Math.floor(m / 60), r = m % 60;
        return (r ? h + ' h ' + r + ' min' : h + (h === 1 ? ' hour' : ' hours')) + ' left';
    }

    // ------------------------------------------------------------------ the map's heading and actions
    function totalIO(M) {
        return '<span class="net-io" title="Total sent and received"><span><span aria-hidden="true">↑</span> <b data-live="tx">' + bytes(M.tx) + '</b></span>' +
            '<span><span aria-hidden="true">↓</span> <b data-live="rx">' + bytes(M.rx) + '</b></span></span>';
    }
    // Activity is Secondary; Announce is Primary. On a phone both are icon-only squares.
    function heroActions(M) {
        var lv = live(M).length > 0, sq = M.phone ? ' ui-btn--icon' : '';
        var word = NV.announcing ? 'Announced' : 'Announce';
        var title = lv ? word : (M.pausedAll ? 'Resume to announce' : 'Connect first');
        return btn('Activity', { act: 'nav-activity', icon: 'activity', cls: sq, iconOnly: M.phone, attrs: ' title="Activity"' }) +
            '<button type="button" class="ui-btn ui-btn--primary netmap-ann' + sq + (NV.announcing ? ' is-success' : '') + '" data-act="announce" title="' + title + '"' + (lv ? '' : ' disabled') + '>' +
            icon(NV.announcing ? 'check' : 'megaphone') + (M.phone ? '<span class="ui-sr">' + word + '</span>' : word) + '</button>';
    }
    function stage(M) {
        var lv = live(M).length;
        var entering = lv && NV.ribbonWas === false;
        NV.ribbonWas = !!lv;
        var ribbon = '<div class="netmap-ribbon' + (lv ? (entering ? ' is-entering' : '') : ' is-empty') + '">' + (lv
            ? '<div class="netmap-ribbon-spark"><span class="netmap-cap">Traffic</span><span data-spark="ribbon">' + spark(NV.hist, 600, 34) + '</span></div>' +
              '<div class="netmap-stat"><b data-live="active">' + M.activePeers + '</b><span>Active peers</span></div>' +
              '<div class="netmap-stat"><b data-live="announce" title="' + (M.lastAnnounce ? K().time.full(M.lastAnnounce) : '') + '">' + (M.lastAnnounce ? ago(Date.now() - M.lastAnnounce) : '—') + '</b><span>Last announce</span></div>'
            : '') + '</div>';
        var open = !NV.mapCollapsed, mapLabel = open ? 'Hide map' : 'Show map';
        var toggle = '<button type="button" class="ui-icon-btn ui-icon-btn--sm ui-icon-btn--disclosure" data-act="map-toggle" aria-expanded="' + open + '" aria-label="' + mapLabel + '" title="' + mapLabel + '">' + disclosure(open) + '</button>';
        // On a phone the pane header carries the title.
        var title = M.phone ? '' : '<h2 class="ui-detail-title">Network</h2>';
        return '<section class="netmap-stage" aria-label="Network map">' +
            '<div class="netmap-hero"><div class="netmap-hero-text">' + title + '<p class="netmap-hero-sub">' + totalIO(M) + '</p></div>' +
            '<div class="netmap-hero-actions">' + (M.phone ? '' : heroActions(M)) + toggle + '</div></div>' +
            (open ? '<div class="netmap-map" id="netmap-map"></div>' + ribbon : '') + '</section>';
    }

    // ------------------------------------------------------------------ Connectivity: one way per kind, one action each
    function tile(kind, on, lg) {
        return '<span class="ui-tile ui-tile--' + KIND[kind].tile + (lg ? ' ui-tile--lg' : '') + (on ? ' net-tile-solid' : '') + '" aria-hidden="true">' + icon(KIND[kind].icon) + '</span>';
    }
    // A way with anything under its head folds shut with its own chevron, on or off.
    function way(kind, o) {
        var fold = o.fold !== false && !!(o.body || o.full || o.foot);
        var shut = fold && NV.waysCollapsed.indexOf(kind) !== -1;
        var verb = shut ? 'Show ' : 'Hide ';
        var chev = fold ? '<button type="button" class="ui-icon-btn ui-icon-btn--sm ui-icon-btn--disclosure" data-act="way-fold" data-id="' + kind + '" aria-expanded="' + !shut +
            '" aria-label="' + esc(verb + o.title) + '" title="' + esc(verb + o.title) + '">' + disclosure(!shut) + '</button>' : '';
        return '<section class="net-way k-' + kind + (o.on ? ' is-on' : '') + (o.start ? ' is-start' : '') + (NV.flash === kind ? ' is-flash' : '') + '" aria-labelledby="net-wt-' + kind + '">' +
            '<div class="net-way-head">' + tile(kind, o.on) +
                '<div class="net-way-titles"><h3 class="net-way-title" id="net-wt-' + kind + '">' + esc(o.title) + '</h3>' +
                '<p class="net-way-status">' + o.status + '</p></div>' +
                (o.ctl || chev ? '<div class="net-way-ctl">' + chev + (o.ctl || '') + '</div>' : '') + '</div>' +
            (o.body && !shut ? '<div class="net-way-body">' + o.body + '</div>' : '') +
            (o.full && !shut ? '<div class="net-way-full">' + o.full + '</div>' : '') +
            (o.foot && !shut ? '<div class="net-way-foot">' + o.foot + '</div>' : '') + '</section>';
    }
    // "Start here" shows until the first connection, and never again after.
    function isStart(M) { return !M.conns.length && !NV.connectedOnce; }
    function onLine(text) { return dot('online') + esc(text); }
    // What keeps Bluetooth Peer from working fully, from the platform's own state.
    function bleNote(M) {
        var word = { bluetooth_off: 'Bluetooth is off', permission_needed: 'Bluetooth permission needed',
            unavailable: 'Not available on this device', central_only: 'On · others can’t find this device' }[M.ble.state];
        return word ? dot('warning') + esc(word) : '';
    }
    function pausedLine() { return dot('') + 'Paused'; }
    function mark(c) {
        if (c.kind === 'tcp' && c.pub && c.pub.group === 'official') return '<span class="net-mark is-gem public-server-card--' + c.pub.tone + '" aria-hidden="true">' + icon('gem') + '</span>';
        if (c.kind === 'tcp') return '<span class="net-mark is-initial" aria-hidden="true">' + esc((c.pub ? c.pub.name : c.name).charAt(0)) + '</span>';
        return '<span class="net-mark" aria-hidden="true">' + icon(KIND[c.kind].icon) + '</span>';
    }
    // A connected row says just "Connected"; the rest is in the detail.
    function rowStatus(c) {
        if (c.scanning) return 'Looking nearby…';
        if (c.status === 'pending') return c.health || 'Connecting…';
        if (c.status === 'paused') return 'Paused';
        if (c.status === 'off') return 'Off';
        return 'Connected';
    }
    function rowName(c) { return c.pub ? c.pub.name : c.name; }
    function crow(c, M) {
        var tone = c.status === 'up' ? 'online' : (c.status === 'paused' || c.status === 'off') ? '' : 'warning';
        return '<div class="net-crow"><button type="button" class="ui-row" data-act="c-node" data-id="' + esc(c.id) + '"' + (!M.phone && NV.sel === 'c:' + c.id ? ' aria-current="true"' : '') +
                ' aria-label="' + esc(rowName(c) + ', ' + rowStatus(c) + '. Show details') + '">' + mark(c) +
                '<span class="ui-row-main"><span class="ui-row-top"><span class="ui-row-title">' + esc(rowName(c)) + '</span></span>' +
                '<span class="ui-row-sub">' + dot(tone) + '<span class="t">' + esc(rowStatus(c)) + '</span></span></span></button>' +
            (c.scanning ? '' : '<button type="button" class="ui-icon-btn ui-icon-btn--sm" data-act="c-menu" data-id="' + esc(c.id) + '" aria-haspopup="menu" aria-expanded="false" aria-label="More for ' + esc(rowName(c)) + '" title="More for ' + esc(rowName(c)) + '">' + icon('more') + '</button>') +
        '</div>';
    }
    function crows(lst, M) { return lst.length ? '<div class="net-crows">' + lst.map(function(c) { return crow(c, M); }).join('') + '</div>' : ''; }
    function serverState(s) {
        return typeof _publicServerState === 'function' ? _publicServerState(s, window._cachedConfigIfaces || {}) : { connected: false, pending: false, match: null };
    }
    // Joining… lasts until the server is reached or the join's wait runs out; a
    // saved server that isn't up then says why and offers Retry.
    function srow(s) {
        var st = serverState(s), act;
        if (st.pending) act = btn('Joining…', { cls: 'ui-btn--sm', spin: true, disabled: true });
        else if (st.connected) act = '<span class="net-joined">' + icon('check') + 'Joined</span>';
        else if (st.configured) act = btn('Retry', { cls: 'ui-btn--sm', act: 'join', attrs: ' data-id="' + esc(s.id) + '" aria-label="Retry ' + esc(s.name) + '"' });
        else act = btn('Join', { cls: 'ui-btn--sm', act: 'join', attrs: ' data-id="' + esc(s.id) + '" aria-label="Join ' + esc(s.name) + '"' });
        var m = s.group === 'official' ? '<span class="net-mark is-gem public-server-card--' + s.tone + '" aria-hidden="true">' + icon('gem') + '</span>'
            : '<span class="net-mark is-initial" aria-hidden="true">' + esc(s.name.charAt(0)) + '</span>';
        var note = st.trouble || s.place;
        return '<div class="net-srow">' + m + '<span class="net-sname">' + esc(s.name) + (note ? '<small>' + esc(note) + '</small>' : '') + '</span>' + act + '</div>';
    }
    function serverList() {
        var all = typeof PUBLIC_TCP_SERVERS !== 'undefined' ? PUBLIC_TCP_SERVERS : [];
        var official = all.filter(function(s) { return s.group === 'official'; });
        var community = all.filter(function(s) { return s.group === 'community'; }).sort(function(a, b) { return a.name.localeCompare(b.name, 'en', { sensitivity: 'base' }); });
        var label = (NV.moreOpen ? 'Hide' : 'Show') + ' unofficial servers';
        var unofficial = '<div class="net-srow net-srow--toggle" data-act="more-servers"><span class="net-mark" aria-hidden="true">' + icon('people') + '</span><span class="net-sname">Unofficial</span>' +
            '<button type="button" class="ui-icon-btn ui-icon-btn--sm ui-icon-btn--disclosure" data-act="more-servers" aria-expanded="' + NV.moreOpen + '" aria-label="' + label + '" title="' + label + '">' + disclosure(NV.moreOpen) + '</button></div>';
        var out = '<div class="net-slist">' + official.map(srow).join('') + unofficial + '</div>';
        if (NV.moreOpen) out += '<p class="ui-help net-snote">Run by the community. Their operators can see your IP address.</p><div class="net-slist">' + community.map(srow).join('') + '</div>';
        return out + '<div class="net-links">' + link('Add your own server', 'own', '', 'plus') + '</div>';
    }
    function pauseAllBtn(M) {
        var active = M.conns.some(function(c) { return c.status === 'up' || c.status === 'pending'; });
        var paused = M.pausedAll;
        return '<button type="button" class="ui-btn ui-btn--quiet" data-act="' + (paused ? 'resume-all' : 'pause-all') + '"' + (paused || active ? '' : ' disabled') +
            ' title="' + (paused ? 'Resume every connection that was on' : active ? 'Pause every connection' : 'Nothing is on') + '">' + icon(paused ? 'play' : 'pause') + (paused ? 'Resume all' : 'Pause all') + '</button>';
    }
    function pausedNotice(M) {
        if (!M.pausedAll) return '';
        var n = M.life.paused_count || 0;
        return '<div class="ui-notice ui-notice--warning" role="status">' + icon('pause') + '<div class="ui-notice-body">' + (n === 1 ? '1 connection' : n + ' connections') + ' paused.</div></div>';
    }
    function deviceWay(M, kind, title, desc) {
        var lst = byKind(M, kind), act = lst.filter(function(c) { return c.status !== 'off'; });
        var n = act.length, word = kind === 'lora' ? (n === 1 ? ' radio' : ' radios') : '';
        var status = !lst.length ? esc(desc) : !n ? 'Off' : allPaused(act) ? pausedLine()
            : act.some(function(c) { return c.status === 'up'; }) ? onLine(kind === 'lora' ? 'On · ' + n + word : 'On · Hosting') : 'Turning on…';
        // A second radio (USB beside Bluetooth, say) is added from the card that lists the first.
        var foot = kind === 'lora' && lst.length ? link('Add another radio', 'add-radio') : '';
        return way(kind, { title: title, on: n > 0, status: status, ctl: sw({ act: 'dev-' + kind, on: n > 0 || NV.setupFor === kind, label: title }), body: crows(lst, M), foot: foot });
    }
    function ways(M) {
        var tcp = byKind(M, 'tcp'), out = '';
        // With no server yet, the switch only opens the server list; with servers, it turns them all off or on.
        if (!tcp.length) {
            out += way('tcp', { title: 'Internet', start: isStart(M), on: NV.internetOpen, fold: false, status: 'Join a server to reach people anywhere.',
                ctl: sw({ act: 'tcp', on: NV.internetOpen, label: 'Internet' }), body: NV.internetOpen ? serverList() : '' });
        } else {
            var tcpOn = tcp.filter(function(c) { return c.status !== 'off'; });
            var up = tcpOn.filter(function(c) { return c.status === 'up'; }).length;
            out += way('tcp', {
                title: 'Internet', on: tcpOn.length > 0,
                status: !tcpOn.length ? 'Off' : allPaused(tcpOn) ? pausedLine() : up ? onLine('On · ' + up + (up === 1 ? ' server' : ' servers')) : 'Joining…',
                ctl: sw({ act: 'tcp', on: tcpOn.length > 0, label: 'Internet' }),
                body: crows(tcp, M) + (NV.joinOpen ? '<div class="net-join-more">' + serverList() + '</div>' : ''),
                foot: link(NV.joinOpen ? 'Hide servers' : 'Join another server', 'join-toggle', ' aria-expanded="' + !!NV.joinOpen + '"')
            });
        }
        var bles = byKind(M, 'ble');
        if (M.ble.on) {
            out += way('ble', {
                title: 'Bluetooth Peer', on: true, status: allPaused(bles) ? pausedLine() : (bleNote(M) || onLine('On · ' + bleLeft(M))),
                ctl: sw({ act: 'ble', on: true, label: 'Bluetooth Peer' }), body: crows(bles, M),
                full: '<div class="net-seg-row"><span class="ui-label">Stay on for</span>' + seg(DURS, M.ble.duration == null ? BLE_DEFAULT_SECS : M.ble.duration, 'Stay on for', 'ble-dur') + '</div>'
            });
        } else {
            out += way('ble', { title: 'Bluetooth Peer', status: M.ble.starting ? 'Turning on…' : (bleNote(M) || 'People nearby, no internet.'),
                ctl: sw({ act: 'ble', on: M.ble.starting, busy: M.ble.starting, label: 'Bluetooth Peer' }) });
        }
        var locals = byKind(M, 'local'), localOn = locals.length > 0;
        out += way('local', {
            title: 'Local Network', on: localOn,
            status: localOn ? (allPaused(locals) ? pausedLine() : locals.some(function(c) { return c.status === 'up'; }) ? onLine('On') : 'Turning on…') : 'Same Wi-Fi or wired network.',
            ctl: sw({ act: 'local', on: localOn, label: 'Local Network' }), body: localOn ? crows(locals, M) : ''
        });
        out += deviceWay(M, 'lora', 'Radio', 'LoRa, long range, off the grid.');
        if (!M.phone || byKind(M, 'host').length) out += deviceWay(M, 'host', 'Host', 'Others connect to this computer.');
        var n = M.disc.entries.length;
        out += '<h3 class="ui-section-title">Find more</h3>' + way('discovered', {
            title: 'Discover servers', on: M.disc.on, status: M.disc.on ? onLine('On · ' + n + ' found') : 'Shared servers and radios.',
            ctl: sw({ act: 'disc', on: M.disc.on, label: 'Discover servers' }),
            foot: M.disc.on ? (n ? link('See all ' + n, 'list') : '') + link('Join automatically: ' + (M.disc.settings.autojoin ? M.disc.settings.autojoin + (M.disc.settings.autojoin === 1 ? ' server' : ' servers') : 'Off'), 'disc-settings') : ''
        });
        return out;
    }

    // ------------------------------------------------------------------ detail, discovered list, menu
    function radioVia(i) {
        var port = String(i.port || '');
        if (port.indexOf('ble://') === 0) return 'Bluetooth';
        if (typeof _rnodeIsTcpPort === 'function' && _rnodeIsTcpPort(port)) return 'Wi-Fi';
        return port ? 'USB' : '';
    }
    function radioMhz(i) { var f = Number(i.frequency) || 0; return f ? (f / 1e6).toFixed(3) + ' MHz' : ''; }
    function sub(c) {
        var i = c.iface || {};
        if (c.kind === 'tcp') return c.pub && c.pub.group === 'official' ? 'Ratspeak server' : (i.target_host || '');
        if (c.kind === 'ble') return 'Nearby by Bluetooth';
        if (c.kind === 'local') return 'This Wi-Fi or wired network';
        if (c.kind === 'lora') return [radioMhz(i), radioVia(i)].filter(Boolean).join(' · ');
        if (c.kind === 'host') return i.listen_port ? 'Port ' + i.listen_port : '';
        return '';
    }
    // What a radio that isn't connecting needs from the person, in its detail.
    function radioTrouble(c) {
        if (c.kind !== 'lora' || c.status === 'paused' || c.status === 'off') return null;
        var i = c.iface || {}, port = String(i.port || ''), ble = port.indexOf('ble://') === 0;
        var hw = ble && window._cachedConfigIfaces && window._cachedConfigIfaces.mobile_hardware ? window._cachedConfigIfaces.mobile_hardware.ble_rnode : null;
        var live = !hw && typeof getInterfaceLiveStatus === 'function' ? getInterfaceLiveStatus(c.id) : null;
        var st = (hw || (live && live.rnode_state)) || {};
        var state = typeof st.state === 'string' ? st.state : '', reason = typeof st.reason === 'string' ? st.reason : '';
        if (!state) return null;
        var help = typeof rnodeConnectionHelp === 'function' ? rnodeConnectionHelp(state, reason, port) : null;
        var inFlight = /^(waiting_for_radio|reconnecting|connecting|initializing)$/.test(state);
        return { help: help, permission: ble && inFlight && reason === 'permission_needed', retry: state === 'failed' || (ble && state === 'disabled') };
    }
    function leaveWord(c) { return c.kind === 'local' ? 'Turn off' : 'Disconnect'; }
    function facts(rows) { return '<dl class="ui-dl">' + rows.map(function(x) { return '<dt>' + x[0] + '</dt><dd>' + x[1] + '</dd>'; }).join('') + '</dl>'; }
    function editable(c) { return !c.joined && (c.kind === 'lora' || c.kind === 'host' || (c.kind === 'tcp' && !c.pub)); }
    function connDetail(c, M) {
        var i = c.iface || {}, f = [];
        if (c.kind === 'tcp' && i.target_host) f.push(['Address', '<span class="ui-hash" data-hash-off>' + esc(i.target_host + ':' + (i.target_port || '')) + '</span>']);
        if (c.kind === 'ble') f.push(['Turns off', M.ble.expiresAt ? 'In ' + bleLeft(M).replace(' left', '') : 'Never']);
        if (c.kind === 'ble' && c.since) f.push(['Connected for', dur(Date.now() - c.since)]);
        if (c.kind === 'local' && i.group_id) f.push(['Network', esc(i.group_id === 'reticulum' ? 'Default' : i.group_id)]);
        if (c.kind === 'local' && c.status === 'up' && c.peers != null) f.push(['Devices nearby', esc(String(c.peers))]);
        if (c.kind === 'host' && c.status === 'up' && c.clients != null) f.push(['Connected clients', esc(String(c.clients))]);
        if (c.kind === 'lora' && radioMhz(i)) f.push(['Frequency', radioMhz(i)]);
        if (c.kind === 'lora' && radioVia(i)) f.push(['Connected by', radioVia(i)]);
        if (c.kind === 'host' && i.listen_port) f.push(['Port', esc(i.listen_port)]);
        if (c.kind === 'host') { var nn = typeof _ifaceIfacNetworkName === 'function' ? _ifaceIfacNetworkName(i) : (i.network_name || ''); f.push(['Network name', nn ? esc(nn) : 'None']); }
        if (c.joined) f.push(['Added by', 'Join automatically']);
        var st = c.status === 'up' ? dot('online') + 'Connected' : c.status === 'paused' ? dot('') + 'Paused' : c.status === 'off' ? dot('') + 'Off' : dot('warning') + esc(rowStatus(c));
        var acts = [];
        // A joined connection isn't saved, so it has nothing to pause; Keep saves it.
        if (c.kind !== 'ble' && c.kind !== 'local' && !c.joined) {
            if (c.status === 'paused' || c.status === 'off') acts.push(btn('Resume', { act: 'op', attrs: ' data-op="resume" data-id="' + esc(c.id) + '"' }));
            else if (c.kind === 'tcp' || c.kind === 'lora') acts.push(btn('Pause', { act: 'op', attrs: ' data-op="pause" data-id="' + esc(c.id) + '"' }));
        }
        var trouble = radioTrouble(c);
        if (trouble && trouble.permission) acts.push(btn('Allow Bluetooth', { act: 'op', attrs: ' data-op="permission" data-id="' + esc(c.id) + '"' }));
        if (trouble && trouble.retry) acts.push(btn('Retry', { act: 'op', attrs: ' data-op="resume" data-id="' + esc(c.id) + '"' }));
        if (c.joined) acts.push(btn('Keep', { act: 'op', attrs: ' data-op="keep" data-id="' + esc(c.id) + '"' }));
        if (editable(c)) acts.push(btn('Edit', { act: 'op', attrs: ' data-op="edit" data-id="' + esc(c.id) + '"' }));
        if (c.kind === 'local') acts.push(btn('Options', { act: 'op', attrs: ' data-op="options" data-id="' + esc(c.id) + '"' }));
        // N9: plain Disconnect closes the row; N15: Local Network gets a plain Turn off.
        if (!c.scanning) acts.push(btn(leaveWord(c), { act: 'op', cls: 'net-push' + (c.kind === 'local' ? '' : ' ui-btn--danger'), attrs: ' data-op="leave" data-id="' + esc(c.id) + '" aria-label="' + leaveWord(c) + ' ' + esc(rowName(c)) + '"' }));
        return '<div class="net-detail"><div class="net-dhead">' + tile(c.kind, false, true) + '<div class="net-dhead-text"><h3 class="ui-detail-title">' + esc(rowName(c)) + '</h3>' +
                '<p class="net-dsub">' + KIND[c.kind].label + (sub(c) ? ' · ' + esc(sub(c)) : '') + '</p></div></div>' +
            '<p class="ui-status net-dstatus">' + st + '</p>' +
            (c.kind === 'ble' ? '' : '<div class="net-io2"><div class="ui-well"><span>Sent</span><b>' + bytes(c.tx) + '</b></div><div class="ui-well"><span>Received</span><b>' + bytes(c.rx) + '</b></div></div>') +
            (f.length ? facts(f) : '') +
            (c.kind === 'host' && RS.hostDiscovery ? '<div class="net-hostdisc" data-host="' + esc(c.id) + '"><h4 class="ui-section-title">Discovery</h4>' + RS.hostDiscovery.html(hostDraft(c), 'hc-') + '</div>' : '') +
            (trouble && trouble.help ? '<div class="ui-notice ui-notice--warning">' + icon('alert') + '<div class="ui-notice-body"><b>' + esc(trouble.help.title) + '.</b> ' + link('Connection help', 'op', ' data-op="help" data-id="' + esc(c.id) + '"') + '</div></div>' : '') +
            '<div class="net-sec">' + link('Open in Activity', 'nav-activity') + '</div>' +
            '<div class="net-acts">' + acts.join('') + '</div></div>';
    }
    function kindWord(e) { return { backbone: 'Backbone server', tcp: 'Server', radio: 'Radio', i2p: 'I2P server' }[e.kind] || 'Other'; }
    function usable(e) {
        if (e.kind === 'radio') return !!(e.frequency && e.bandwidth && e.spreading_factor && e.coding_rate);
        return !!e.connectable || !!e.joined_interface;
    }
    function heardAgo(e) { return e.last_heard ? ago(Date.now() - e.last_heard * 1000) : 'not yet'; }
    function ddot(e) { return '<span class="net-ddot' + (e.status === 'available' ? '' : ' is-' + esc(e.status)) + '" aria-hidden="true"></span>'; }
    function discDetail(e) {
        var mhz = function(hz) { return (Math.round((hz || 0) / 1e3) / 1e3).toFixed(3); };
        var f = e.kind === 'radio'
            ? [['Frequency', mhz(e.frequency) + ' MHz'], ['Bandwidth', e.bandwidth ? Math.round(e.bandwidth / 1000) + ' kHz' : 'Not shared'], ['Spreading factor', e.spreading_factor || 'Not shared'], ['Coding rate', e.coding_rate || 'Not shared']]
            : [['Address', '<span class="ui-hash" data-hash-off>' + esc((e.address || '') + (e.port ? ':' + e.port : '')) + '</span>']];
        if (e.own_network) f.push(['Access', e.private_network ? 'Filled in when you join' : 'Open']);
        f.push(['Distance', e.hops + (e.hops === 1 ? ' hop away' : ' hops away')], ['Heard', (e.heard_count || 1) + ((e.heard_count || 1) === 1 ? ' time' : ' times') + ', last ' + heardAgo(e)],
            ['Published by', K().hash.copy(e.publisher, { noun: 'Publisher identity hash' }) + (e.trusted ? ' <span class="ui-tag ui-tag--accent">Trusted</span>' : '')]);
        var heardWord = e.status === 'stale' ? 'Not heard lately' : e.status === 'unknown' ? 'Heard in the last 3 days' : 'Heard today';
        var act = e.joined_interface ? btn('Keep', { act: 'disc-keep', cls: 'ui-btn--primary', attrs: ' data-id="' + esc(e.id) + '"' })
            : e.added ? '<span class="net-joined">' + icon('check') + 'Saved</span>'
            : !usable(e) ? '<p class="net-quiet-text">' + (e.kind === 'radio' ? 'It doesn’t share all its radio settings.' : 'This device can’t reach its address.') + '</p>'
            : e.kind === 'radio' ? btn('Use these radio settings', { act: 'disc-radio', cls: 'ui-btn--primary', attrs: ' data-id="' + esc(e.id) + '"' })
            : btn('Join', { act: 'disc-join', cls: 'ui-btn--primary', attrs: ' data-id="' + esc(e.id) + '"' });
        return '<div class="net-detail"><div class="net-dhead">' + tile('discovered', false, true) + '<div class="net-dhead-text"><h3 class="ui-detail-title">' + esc(e.name || 'Unnamed') + '</h3>' +
                '<p class="net-dsub">Discovered · ' + kindWord(e) + (e.own_network ? ' · Private' : '') + '</p></div></div>' +
            '<p class="ui-status net-dstatus">' + ddot(e) + heardWord + '</p>' + facts(f) +
            (e.own_network ? '<div class="ui-notice">' + icon('lock') + '<div class="ui-notice-body">From your private network. Joining fills in its network name and passphrase.</div></div>'
                : e.kind === 'radio' || !usable(e) || e.added ? '' : '<div class="ui-notice">' + icon('info') + '<div class="ui-notice-body">Its operator can see your IP address once you join.</div></div>') +
            '<div class="net-acts">' + act + (e.trusted || e.own_network ? '' : link('Trust this publisher', 'disc-trust', ' data-id="' + esc(e.id) + '"')) + '</div></div>';
    }
    function drow(M, e) {
        var meta = [{ backbone: 'Backbone', tcp: 'Server', radio: 'Radio', i2p: 'I2P' }[e.kind] || 'Other'];
        meta.push(e.kind === 'radio' ? (Math.round((e.frequency || 0) / 1e3) / 1e3).toFixed(3) + ' MHz' : e.hops + (e.hops === 1 ? ' hop' : ' hops'));
        if (e.last_heard) meta.push('heard ' + heardAgo(e));
        var act;
        if (e.joined || e.added) act = '<span class="net-joined">' + icon('check') + (e.joined ? 'Joined' : 'Saved') + '</span>';
        else if (!usable(e)) act = '<span class="net-quiet-text">' + (e.kind === 'radio' ? 'Incomplete' : 'Unreachable') + '</span>';
        else if (e.kind === 'radio') act = btn('Use', { cls: 'ui-btn--sm', act: 'disc-radio', attrs: ' data-id="' + esc(e.id) + '" aria-label="Use ' + esc(e.name) + ' radio settings"' });
        else act = btn('Join', { cls: 'ui-btn--sm', act: 'disc-join', attrs: ' data-id="' + esc(e.id) + '" aria-label="Join ' + esc(e.name) + '"' });
        return '<div class="net-drow' + (e.status === 'stale' ? ' is-stale' : '') + '"><button type="button" class="ui-row" data-act="d-sel" data-id="' + esc(e.id) + '"' + (!M.phone && NV.sel === 'd:' + e.id ? ' aria-current="true"' : '') + '>' + ddot(e) +
            '<span class="ui-row-main"><span class="ui-row-top"><span class="ui-row-title">' + esc(e.name || 'Unnamed') + '</span>' + (e.own_network ? '<span class="ui-tag ui-tag--accent">Private</span>' : e.trusted ? '<span class="ui-tag ui-tag--accent">Trusted</span>' : '') + '</span>' +
            '<span class="ui-row-sub"><span class="t">' + esc(meta.join(' · ')) + '</span></span></span></button>' + act + '</div>';
    }
    // N1: the runtime sends the list in the agreed ranking; no rank numbers.
    function discList(M) {
        var all = M.disc.entries, f = NV.dfilter;
        var pass = function(e) { return f === 'all' || (f === 'radio' ? e.kind === 'radio' : e.kind !== 'radio'); };
        var servers = all.filter(function(e) { return e.kind !== 'radio'; }).length;
        var fresh = all.filter(function(e) { return (e.status !== 'stale' || e.joined || e.added) && pass(e); });
        var stale = all.filter(function(e) { return e.status === 'stale' && !e.joined && !e.added && pass(e); });
        var rows = function(lst) { return '<div class="net-dlist">' + lst.map(function(e) { return drow(M, e); }).join('') + '</div>'; };
        return '<div class="net-list-top">' + (M.phone ? '' : '<h3 class="ui-detail-title">Discovered <span class="ui-pane-count">' + all.length + '</span></h3>') +
                '<p class="net-dsub">Servers and radios that others share on the network.</p>' +
                seg([['all', 'All ' + all.length], ['server', 'Servers ' + servers], ['radio', 'Radios ' + (all.length - servers)]], f, 'Show', 'dfilter') + '</div>' +
            (fresh.length ? rows(fresh) : '<div class="ui-empty ui-empty--quiet"><h4 class="ui-empty-title">Nothing heard in the last 3 days</h4></div>') +
            '<div class="net-list-foot">' + (stale.length ? (NV.older ? '<p class="ui-caption">Not heard for over 3 days</p>' + rows(stale) : link('Show ' + stale.length + ' not heard lately', 'disc-older')) : '') +
                link('Join automatically: ' + (M.disc.settings.autojoin ? M.disc.settings.autojoin + (M.disc.settings.autojoin === 1 ? ' server' : ' servers') : 'Off'), 'disc-settings') + '</div>';
    }
    function menuItems(c) {
        var items = [{ label: 'Details', icon: 'info', op: 'details' }];
        if ((c.kind === 'tcp' || c.kind === 'lora') && c.status !== 'off' && !c.joined) items.push(c.status === 'paused' ? { label: 'Resume', icon: 'play', op: 'resume' } : { label: 'Pause', icon: 'pause', op: 'pause' });
        if (c.joined) items.push({ label: 'Keep', icon: 'check', op: 'keep' });
        if (editable(c)) items.push({ label: 'Edit', icon: 'edit', op: 'edit' });
        if (c.kind === 'local') items.push({ label: 'Options', icon: 'sliders', op: 'options' });
        if (c.kind === 'local') items.push({ label: 'Turn off', icon: 'power', op: 'leave' });
        else items.push({ separator: true }, { label: leaveWord(c), icon: 'leave', op: 'leave', danger: true });
        return items;
    }

    // ------------------------------------------------------------------ the side panel and the screen
    function selected(M) {
        if (!NV.sel) return null;
        var id = NV.sel.slice(2);
        if (NV.sel.charAt(0) === 'c') { var c = byId(M, id); return c ? { type: 'c', item: c } : null; }
        var e = M.disc.entries.filter(function(x) { return x.id === id; })[0];
        return e ? { type: 'd', item: e } : null;
    }
    function backBtn(label, act) { return '<button type="button" class="ui-btn ui-btn--quiet ui-btn--sm" data-act="' + act + '">' + icon('chevron-left') + esc(label) + '</button>'; }
    function inspector(M) {
        var s = M.phone ? null : selected(M);
        if (s) {
            var fromList = s.type === 'd' && NV.view === 'list';
            return '<aside class="netmap-insp" aria-label="Details"><div class="netmap-insp-scroll"><div class="netmap-insp-head has-back">' + (fromList ? backBtn('Discovered', 'back-list') : backBtn('Connectivity', 'back')) + '</div>' +
                (s.type === 'c' ? connDetail(s.item, M) : discDetail(s.item)) + '</div></aside>';
        }
        if (!M.phone && NV.view === 'list' && M.disc.on) {
            return '<aside class="netmap-insp" aria-label="Discovered"><div class="netmap-insp-scroll"><div class="netmap-insp-head has-back">' + backBtn('Connectivity', 'back') + '</div>' + discList(M) + '</div></aside>';
        }
        return '<aside class="netmap-insp" aria-labelledby="netmap-insp-t"><div class="netmap-insp-scroll"><div class="netmap-insp-head"><h2 class="ui-card-title" id="netmap-insp-t">Connectivity</h2>' + pauseAllBtn(M) + '</div>' +
            '<div class="netmap-ways">' + pausedNotice(M) + ways(M) + '</div></div></aside>';
    }
    function view(M) {
        var body = '<div class="netmap' + (NV.mapCollapsed ? ' is-collapsed' : '') + '">' + stage(M) + inspector(M) + '</div>';
        if (!M.phone) return body;
        return '<header class="ui-pane-head netmap-head"><h1 class="ui-pane-title">Network</h1><div class="ui-pane-actions">' + heroActions(M) + '</div></header>' +
            '<div class="netmap-scroll ui-scroll" id="netmap-scroll">' + body + '</div>';
    }

    // ------------------------------------------------------------------ the map
    var D2R = Math.PI / 180, SLOT = { ble: 6, local: 64, lora: 116, host: 174 }, SLOT_PHONE = { ble: 8, local: 90, lora: 172 };
    var STRIP_H = { desk: 88, phone: 80 }, MIN_GAP = 16;
    function nodesFor(M, r1) {
        var nodes = [], tcp = byKind(M, 'tcp'), phone = M.phone;
        var spread = Math.min(58, 2 * Math.asin(Math.min(0.95, (phone ? 55 : 78) / r1)) / D2R);
        if (tcp.length) tcp.forEach(function(c, i) { nodes.push({ c: c, kind: 'tcp', a: -90 + (i - (tcp.length - 1) / 2) * spread }); });
        else nodes.push({ ghost: true, kind: 'tcp', a: -90, start: isStart(M) });
        ['ble', 'local', 'lora', 'host'].forEach(function(k) {
            if (k === 'host' && phone) return;
            var lst = byKind(M, k), at = (phone ? SLOT_PHONE : SLOT)[k];
            if (lst.length) lst.forEach(function(c, i) { nodes.push({ c: c, kind: k, a: at + i * 22 }); });
            else nodes.push({ ghost: true, kind: k, a: at, busy: k === 'ble' && M.ble.starting });
        });
        return nodes;
    }
    function ghostWord(n) { return n.busy ? 'Turning on…' : n.kind === 'tcp' ? 'Join a server' : 'Off'; }
    function nodeName(c) { return c.scanning ? KIND[c.kind].label : rowName(c); }
    function layoutMap(M, shrink) {
        var map = document.getElementById('netmap-map');
        if (!map) return;
        var phone = M.phone, w = map.clientWidth, h = map.clientHeight;
        if (!w || !h) return;
        var hMin = h - (document.querySelector('.netmap-ribbon') ? 0 : STRIP_H[phone ? 'phone' : 'desk']);
        var r1 = Math.max(60, Math.min(w, hMin) * (phone ? 0.33 : 0.31) - (shrink || 0)), youR = phone ? 30 : 38, nodeR = phone ? 22 : 26;
        var ph = Math.round(2 * r1 + 200), cx = w / 2, cy = ph / 2;
        var nodes = nodesFor(M, r1), spokes = [];
        var svg = '<svg class="netmap-svg" width="' + w + '" height="' + ph + '" viewBox="0 0 ' + w + ' ' + ph + '" style="width:' + w + 'px;height:' + ph + 'px" aria-hidden="true"><circle class="netmap-ring" cx="' + cx + '" cy="' + cy + '" r="' + r1 + '"/>', html = '';
        nodes.forEach(function(n) {
            var a = n.a * D2R;
            n.x = cx + Math.cos(a) * r1; n.y = cy + Math.sin(a) * r1;
            var x1 = cx + Math.cos(a) * youR, y1 = cy + Math.sin(a) * youR, x2 = cx + Math.cos(a) * (r1 - nodeR), y2 = cy + Math.sin(a) * (r1 - nodeR);
            if (n.ghost) svg += '<line class="netmap-ghost-spoke" x1="' + x1 + '" y1="' + y1 + '" x2="' + x2 + '" y2="' + y2 + '"/>';
            else {
                var st = n.c.status;
                svg += '<line class="netmap-spoke k-' + n.kind + (st === 'pending' ? ' is-pending' : (st === 'paused' || st === 'off') ? ' is-paused' : '') + '" data-id="' + esc(n.c.id) + '" x1="' + x1 + '" y1="' + y1 + '" x2="' + x2 + '" y2="' + y2 + '"/>';
                spokes.push({ id: n.c.id, kind: n.kind, up: st === 'up' && !n.c.scanning, x1: x1, y1: y1, x2: x2, y2: y2, nx: n.x, ny: n.y, nodeR: nodeR });
            }
            var sel = n.c && NV.sel === 'c:' + n.c.id && !phone;
            var cls = 'netmap-node k-' + n.kind + (n.ghost ? ' is-ghost' : '') + (n.start ? ' is-start' : '') + (sel ? ' is-sel' : '') +
                (n.c && (n.c.status === 'paused' || n.c.status === 'off') ? ' is-paused' : '') + (n.c && n.c.status === 'pending' ? ' is-pending' : '');
            var top = Math.sin(n.a * D2R) < -0.3;
            var label = n.ghost
                ? '<b>' + KIND[n.kind].label + '</b>' + (n.start ? '<em>Start here</em>' : '<span>' + ghostWord(n) + '</span>')
                : '<b>' + esc(nodeName(n.c)) + '</b>' + (n.c.status === 'up' && !n.c.scanning ? '' : '<span>' + (n.c.scanning ? 'Looking nearby…' : n.c.status === 'paused' ? 'Paused' : n.c.status === 'off' ? 'Off' : 'Connecting…') + '</span>');
            var aria = n.ghost ? KIND[n.kind].label + ', ' + (n.start ? 'start here' : ghostWord(n)) : nodeName(n.c) + ', ' + KIND[n.kind].label;
            html += '<div class="' + cls + '" style="left:' + n.x.toFixed(1) + 'px;top:' + n.y.toFixed(1) + 'px">' +
                '<button type="button" class="netmap-node-btn" data-act="' + (n.ghost ? 'c-ghost' : 'c-node') + '" data-id="' + esc(n.ghost ? n.kind : n.c.id) + '" aria-label="' + esc(aria) + '"' + (sel ? ' aria-pressed="true"' : '') + '>' +
                icon(KIND[n.kind].icon) + '</button><span class="netmap-label ' + (top ? 'at-top' : 'at-bottom') + '">' + label + '</span></div>';
        });
        svg += '</svg>';
        var you = '<div class="netmap-you" data-on="' + (live(M).length > 0) + '" style="left:' + cx + 'px;top:' + (cy + (phone ? 0 : 14)) + 'px">' +
            '<span class="netmap-you-ring" style="margin-top:' + (phone ? 0 : -28) + 'px">' + K().avatar(M.me.hash, phone ? 32 : 40) + '</span>' +
            (phone ? '' : '<span class="netmap-you-name">' + esc(M.me.name) + '</span>') + '</div>';
        map.innerHTML = '<div class="netmap-plane" style="width:' + w + 'px;height:' + ph + 'px">' + svg + you + html + '</div>';
        var plane = map.firstChild, pr = plane.getBoundingClientRect(), mr = map.getBoundingClientRect(), lo = Infinity, hi = -Infinity;
        plane.querySelectorAll('.netmap-node-btn, .netmap-label, .netmap-you').forEach(function(el) { var r = el.getBoundingClientRect(); lo = Math.min(lo, r.top - pr.top); hi = Math.max(hi, r.bottom - pr.top); });
        var subEl = document.querySelector('#view-network .netmap-hero-sub'), upper = subEl ? Math.max(0, subEl.getBoundingClientRect().bottom - mr.top) : 0;
        var free = (h - upper - (hi - lo)) / 2;
        if (free < MIN_GAP && (shrink || 0) < 200 && r1 > 60) { layoutMap(M, (shrink || 0) + (MIN_GAP - free) + 1); return; }
        var ty = Math.round(upper + free - lo), prev = NV.planeTY, prevW = NV.mapW;
        NV.planeTY = ty; NV.mapW = w;
        if (prev != null && prev !== ty && prevW === w && !anim.reduced()) {
            plane.style.transform = 'translateY(' + prev + 'px)'; plane.getBoundingClientRect();
            plane.style.transition = 'transform .3s ease';
        }
        plane.style.transform = 'translateY(' + ty + 'px)';
        map.setAttribute('data-w', w); map.setAttribute('data-h', h);
        anim.attach(plane, spokes, w, ph);
    }

    // One loop, one canvas. It runs only while Network shows, the page is visible, the map is open, and something moves.
    var anim = (function() {
        var canvas = null, ctx = null, W = 0, H = 0, spokes = [], dots = [], rings = [], nextEmit = {}, raf = 0, running = false, KC = {};
        var mql = window.matchMedia ? window.matchMedia('(prefers-reduced-motion: reduce)') : null;
        var TRAFFIC_R = 2.6, HARD_CAP = 40;
        function cap() { return compact() ? 12 : 24; }
        function reduced() { return !!(mql && mql.matches); }
        function visible() { return document.visibilityState === 'visible' && typeof isViewActive === 'function' && isViewActive('network') && !NV.mapCollapsed; }
        function traffic() { return dots.filter(function(d) { return d.type === 't'; }).length; }
        function wants() { return !!canvas && visible() && canvas.isConnected && (dots.length > 0 || rings.length > 0 || (!reduced() && spokes.some(function(s) { return s.up; }))); }
        function readColors() {
            var cs = getComputedStyle(document.documentElement), v = function(n) { return cs.getPropertyValue(n).trim(); };
            KC = { tcp: v('--status-info'), ble: v('--status-purple'), local: v('--status-warning'), lora: v('--status-online'), host: v('--text-secondary'), ok: v('--status-online-fg') };
        }
        function ensureCanvas() {
            if (canvas) return;
            canvas = document.createElement('canvas');
            canvas.className = 'netmap-canvas';
            canvas.setAttribute('aria-hidden', 'true');
            ctx = canvas.getContext('2d');
        }
        function attach(plane, lst, w, h) {
            ensureCanvas();
            if (!ctx) return;
            var byIdS = {};
            lst.forEach(function(sp) { byIdS[sp.id] = sp; });
            spokes = lst; W = w; H = h;
            var dpr = Math.min(2, window.devicePixelRatio || 1);
            canvas.width = Math.round(w * dpr); canvas.height = Math.round(h * dpr);
            canvas.style.width = w + 'px'; canvas.style.height = h + 'px';
            ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
            readColors();
            var svg = plane.querySelector('.netmap-svg');
            plane.insertBefore(canvas, svg ? svg.nextSibling : plane.firstChild);
            // Dots in flight keep their place on the new geometry; dots on a removed or paused spoke go.
            dots = dots.filter(function(d) { return byIdS[d.s.id] && (byIdS[d.s.id].up || d.type === 'a'); });
            dots.forEach(function(d) { d.s = byIdS[d.s.id]; });
            rings = [];
            ensure();
        }
        function start() { if (running || !wants()) return; running = true; raf = requestAnimationFrame(frame); }
        function stop() {
            if (raf) cancelAnimationFrame(raf);
            raf = 0; running = false; dots = []; rings = []; nextEmit = {};
            if (ctx) ctx.clearRect(0, 0, W, H);
        }
        function ensure() { if (wants()) start(); else stop(); }
        function frame(ts) {
            raf = 0;
            if (!running) return;
            if (!wants()) { stop(); return; }
            if (!reduced()) {
                spokes.forEach(function(sp) {
                    if (!sp.up) return;
                    if (nextEmit[sp.id] == null) { nextEmit[sp.id] = ts + Math.random() * 700; return; }
                    if (ts < nextEmit[sp.id]) return;
                    nextEmit[sp.id] = ts + 500 + Math.random() * 700;
                    if (traffic() >= cap() || dots.length >= HARD_CAP) return;
                    var inbound = Math.random() < 0.62;
                    dots.push({ s: sp, t0: ts, dur: 1500 + Math.random() * 600, dir: inbound ? -1 : 1, r: inbound ? TRAFFIC_R : 2.2, a: inbound ? 0.95 : 0.6, type: 't' });
                });
            }
            ctx.clearRect(0, 0, W, H);
            dots = dots.filter(function(d) {
                var p = (ts - d.t0) / d.dur;
                if (p < 0) return true;
                if (p >= 1) { if (d.type === 'a') rings.push({ x: d.s.nx, y: d.s.ny, r0: d.s.nodeR, t0: ts }); return false; }
                var q = d.type === 'a' ? 1 - Math.pow(1 - p, 3) : p;
                var fx = d.dir > 0 ? d.s.x1 : d.s.x2, fy = d.dir > 0 ? d.s.y1 : d.s.y2, tx = d.dir > 0 ? d.s.x2 : d.s.x1, ty = d.dir > 0 ? d.s.y2 : d.s.y1;
                ctx.globalAlpha = d.a * Math.max(0, Math.min(1, p / 0.12, (1 - p) / 0.12));
                ctx.fillStyle = d.type === 'a' ? KC.ok : (KC[d.s.kind] || KC.tcp);
                ctx.beginPath(); ctx.arc(fx + (tx - fx) * q, fy + (ty - fy) * q, d.r, 0, 6.2832); ctx.fill();
                return true;
            });
            rings = rings.filter(function(g) {
                var p = (ts - g.t0) / 520;
                if (p >= 1) return false;
                ctx.globalAlpha = 0.8 * (1 - p); ctx.strokeStyle = KC.ok; ctx.lineWidth = 2;
                ctx.beginPath(); ctx.arc(g.x, g.y, g.r0 + 2 + p * 12, 0, 6.2832); ctx.stroke();
                return true;
            });
            ctx.globalAlpha = 1;
            raf = requestAnimationFrame(frame);
        }
        // Announce: one green dot, twice a traffic dot, from this device out along every live spoke.
        function announce() {
            var lv = spokes.filter(function(sp) { return sp.up; });
            if (!lv.length || NV.mapCollapsed) return;
            if (reduced()) {
                lv.forEach(function(sp) {
                    var n = document.querySelector('.netmap-node-btn[data-id="' + (window.CSS && CSS.escape ? CSS.escape(sp.id) : sp.id) + '"]');
                    if (n) n.parentNode.classList.add('is-ann');
                });
                setTimeout(function() { document.querySelectorAll('.netmap-node.is-ann').forEach(function(n) { n.classList.remove('is-ann'); }); }, 1200);
                return;
            }
            var now = performance.now();
            lv.forEach(function(sp, i) { dots.push({ s: sp, t0: now + i * 40, dur: 720, dir: 1, r: TRAFFIC_R * 2, a: 1, type: 'a' }); });
            ensure();
        }
        document.addEventListener('visibilitychange', ensure);
        if (mql && mql.addEventListener) mql.addEventListener('change', ensure);
        return { attach: attach, ensure: ensure, announce: announce, reduced: reduced, detach: function() { spokes = []; stop(); }, running: function() { return running; } };
    })();
    NV.anim = anim;

    // ------------------------------------------------------------------ render and update
    var root = null, layer = null, mapObs = null, lastSig = '';
    function signature(M) {
        return JSON.stringify([M.phone, M.conns.map(function(c) { return [c.id, c.status, c.name, c.health]; }), M.pausedAll, M.life.paused_count, M.ble.on, M.ble.starting,
            M.ble.duration, Math.ceil((M.ble.expiresAt * 1000 - Date.now()) / MIN), M.disc.on, M.disc.entries.length, M.disc.settings.autojoin, NV.sel, NV.view, NV.dfilter, NV.older,
            NV.moreOpen, NV.joinOpen, NV.mapCollapsed, NV.announcing, NV.setupFor, NV.internetOpen, NV.waysCollapsed.join(), NV.connectedOnce, live(M).length > 0, M.disc.entries.map(function(e) { return [e.id, e.joined, e.added, e.status]; })]);
    }
    function render(force) {
        root = root || document.getElementById('network-root');
        if (!root || !K()) return;
        var M = model();
        if (M.conns.length && !NV.connectedOnce) {
            NV.connectedOnce = true;
            RS.invoke('set_ui_state', { key: 'network_connected_once', value: true }).catch(function() {});
        }
        var sig = signature(M);
        if (!force && sig === lastSig && root.childElementCount) { update(M); return; }
        lastSig = sig;
        var sc = root.querySelector('.netmap-insp-scroll'), keep = sc ? sc.scrollTop : 0;
        var ps = document.getElementById('netmap-scroll'), keepP = ps ? ps.scrollTop : 0;
        if (NV.sel && !selected(M)) NV.sel = null;
        if (!M.disc.on && NV.view === 'list') NV.view = null;
        root.innerHTML = view(M);
        sizeTotals();
        if (!NV.mapCollapsed) layoutMap(M); else anim.detach();
        var sc2 = root.querySelector('.netmap-insp-scroll'); if (sc2 && !NV.jump) sc2.scrollTop = keep;
        var ps2 = document.getElementById('netmap-scroll'); if (ps2) ps2.scrollTop = keepP;
        NV.jump = false;
        if (mapObs) { mapObs.disconnect(); var mp = document.getElementById('netmap-map'); if (mp) mapObs.observe(mp); }
    }
    // Stats samples change numbers and the strip only; nothing reflows.
    function update(M) {
        var set = function(sel, text) { var el = root.querySelector(sel); if (el && el.textContent !== text) el.textContent = text; };
        set('[data-live="tx"]', bytes(M.tx));
        set('[data-live="rx"]', bytes(M.rx));
        set('[data-live="active"]', String(M.activePeers));
        set('[data-live="announce"]', M.lastAnnounce ? ago(Date.now() - M.lastAnnounce) : '—');
        var s = root.querySelector('[data-spark="ribbon"]');
        if (s) s.innerHTML = spark(NV.hist, 600, 34);
    }
    // Each total and stat gets a slot as wide as its widest value in this font, so values never shift the line.
    function slot(b, widest, prop) {
        if (!b) return;
        var probe = document.createElement('b');
        probe.style.cssText = 'position:absolute;visibility:hidden;white-space:nowrap;min-width:0';
        b.parentNode.appendChild(probe);
        var wMax = 0;
        widest.forEach(function(t) { probe.textContent = t; wMax = Math.max(wMax, probe.getBoundingClientRect().width); });
        probe.remove();
        if (wMax) document.documentElement.style.setProperty(prop, Math.ceil(wMax + 1) + 'px');
    }
    function sizeTotals() {
        if (!root) return;
        slot(root.querySelector('.net-io b'), BYTES_WIDEST, '--net-io-slot');
        slot(root.querySelector('[data-live="active"]'), ['99999'], '--net-count-slot');
        slot(root.querySelector('[data-live="announce"]'), AGO_WIDEST, '--net-ago-slot');
    }
    function sampleTraffic() {
        var stats = typeof lastStats !== 'undefined' && lastStats ? lastStats : null;
        if (!stats || !stats.interface_stats || typeof interfaceStatsTotals !== 'function') return;
        var t = interfaceStatsTotals(stats.interface_stats.interfaces || []), total = (t.txb || 0) + (t.rxb || 0);
        if (NV.lastTotal != null) {
            NV.hist.push(Math.max(0, total - NV.lastTotal));
            if (NV.hist.length > 60) NV.hist.shift();
        }
        NV.lastTotal = total;
    }
    NV.render = function(force) { if (typeof isViewActive !== 'function' || isViewActive('network')) render(force); };
    NV.onStats = function() { sampleTraffic(); NV.render(); };

    // ------------------------------------------------------------------ actions
    function toast(msg, tone) { if (typeof showToast === 'function') showToast(msg, tone || 'toast-success', 3000); }
    function fail(err, fallback) { if (typeof showToast === 'function') showToast((err && err.message) || fallback, 'toast-error', 6000); }
    function refresh() { if (typeof refreshConfigInterfaces === 'function') refreshConfigInterfaces(); }
    function openConn(id) {
        NV.sel = 'c:' + id; NV.jump = true;
        var M = model(), c = byId(M, id);
        if (!c) return;
        if (M.phone) {
            RS.kit.dialog({ title: rowName(c), size: 'md', body: connDetail(c, M) }).dialog.addEventListener('click', onClick);
            return;
        }
        render(true);
    }
    function openDisc(id, from) {
        NV.sel = 'd:' + id; NV.jump = true;
        var M = model(), e = M.disc.entries.filter(function(x) { return x.id === id; })[0];
        if (!e) return;
        if (M.phone) {
            RS.kit.dialog({ title: e.name || 'Discovered', size: 'md', body: discDetail(e) }).dialog.addEventListener('click', onClick);
            return;
        }
        if (from) NV.view = 'list';
        render(true);
    }
    function openList() {
        var M = model();
        if (M.phone) {
            NV.listSheet = RS.kit.dialog({ title: 'Discovered', size: 'md', body: discList(M), onClose: function() { NV.listSheet = null; } });
            NV.listSheet.dialog.addEventListener('click', onClick);
            return;
        }
        NV.view = 'list'; NV.sel = null; NV.jump = true; render(true);
    }
    function connOp(op, id, trigger) {
        var M = model(), c = byId(M, id);
        if (!c) return;
        if (op === 'details') { openConn(id); return; }
        if (c.kind === 'ble') {
            if (op === 'leave') {
                if (c.peer && c.peer.addresses) c.peer.addresses.forEach(function(address) { RS.invoke('disconnect_ble_peer', { address: address }).catch(function() {}); });
                else disableBle();
            }
            return;
        }
        if (op === 'help') { var t = radioTrouble(c); if (t && t.help && typeof showBleRnodeHelp === 'function') showBleRnodeHelp(t.help, function() { connOp('edit', id); }); return; }
        if (op === 'permission') { if (typeof requestBleRnodePermission === 'function') requestBleRnodePermission(); return; }
        if (op === 'pause' || op === 'resume') { if (typeof setInterfacePaused === 'function') setInterfacePaused(c.ifaceType, c.id, op === 'pause'); return; }
        if (op === 'edit') { if (typeof openInterfaceEdit === 'function') openInterfaceEdit(c.ifaceType, c.id); return; }
        if (op === 'keep') { if (RS.discovery && RS.discovery.keep) RS.discovery.keep(c.id); return; }
        if (op === 'options') { if (typeof showAutoInterfaceConfigSheet === 'function') showAutoInterfaceConfigSheet(); return; }
        if (op === 'leave') {
            if (c.kind === 'local') { RS.invoke('disable_auto_interface', c.ifaceType === 'auto' ? { name: c.id } : {}).then(function() { toast('Local Network is off'); }).catch(function(err) { fail(err, 'Could not turn off Local Network'); }); return; }
            if (typeof confirmRemoveInterface === 'function') confirmRemoveInterface(c.ifaceType, c.id);
        }
    }
    function pauseAll() {
        RS.invoke('pause_all_interfaces').then(function(r) {
            var n = (r && r.paused) || 0;
            toast(n === 1 ? 'Paused 1 connection.' : 'Paused all ' + n + ' connections.');
            refresh();
        }).catch(function(err) { fail(err, 'Could not pause connections'); });
    }
    function resumeAll() {
        RS.invoke('resume_all_interfaces').then(function(r) {
            var n = (r && r.resumed) || 0;
            toast('Resumed ' + n + (n === 1 ? ' connection.' : ' connections.'));
            refresh();
        }).catch(function(err) { fail(err, 'Could not resume connections'); });
    }
    // Bluetooth Peer switches on for 30 minutes; the time choice shows while it's on.
    function enableBle() {
        RS.invoke('api_ble_peer_available').then(function(data) {
            window._blePeerAvailable = !!(data && data.available);
            if (data && data.available) return true;
            var missing = (data && data.missing) || [];
            if (typeof hasAndroidBridge === 'function' && hasAndroidBridge() && typeof _ensureBlePermissions === 'function') return _ensureBlePermissions();
            if (typeof showToast === 'function') showToast(missing.length ? 'Bluetooth Peer isn’t available: ' + missing.join(', ') : 'Bluetooth Peer isn’t available. No Bluetooth adapter found.', 'toast-warning', 5000);
            return false;
        }).then(function(ok) {
            if (!ok) { NV.bleStarting = false; render(true); return; }
            if (typeof _ensureBlePermissions === 'function' && !(typeof hasAndroidBridge === 'function' && hasAndroidBridge())) return _ensureBlePermissions().then(go);
            return go(true);
            function go(granted) {
                if (!granted) { NV.bleStarting = false; render(true); return; }
                return RS.invoke('enable_ble_peer_interface', { args: { duration: BLE_DEFAULT_SECS } }).catch(function(err) {
                    NV.bleStarting = false; render(true); fail(err, 'Could not turn on Bluetooth Peer');
                });
            }
        }).catch(function() { NV.bleStarting = false; render(true); });
        NV.bleStarting = true;
        render(true);
    }
    function disableBle() {
        RS.invoke('disable_ble_peer_interface').then(function() { toast('Bluetooth Peer is off'); }).catch(function(err) { fail(err, 'Could not turn off Bluetooth Peer'); });
    }
    // N3: a new time counts from the moment of the change.
    function setBleDuration(secs, label) {
        RS.invoke('set_ble_peer_duration', { args: { duration: secs } }).then(function(r) {
            window._blePeerDuration = r && r.duration != null ? r.duration : secs;
            window._blePeerExpiresAt = r && r.expires_at != null ? r.expires_at : 0;
            toast(secs ? 'Bluetooth Peer stays on for ' + label + ' from now.' : 'Bluetooth Peer stays on until you turn it off.');
            render(true);
        }).catch(function(err) { fail(err, 'Could not change the time'); });
    }
    // Radio and Host: with none set up, the switch opens setup and goes back off if setup closes empty.
    function deviceSwitch(kind, on) {
        var M = model(), lst = byKind(M, kind);
        if (!lst.length) {
            // Off while setup is still open: the switch goes off now, whatever setup does next.
            if (!on) { NV.setupFor = null; render(true); return; }
            NV.setupFor = kind; render(true);
            var done = function() { if (NV.setupFor === kind) { NV.setupFor = null; render(true); } };
            if (kind === 'lora') { if (typeof openRnodeModal === 'function') openRnodeModal('ble'); }
            else if (typeof openHostModal === 'function') openHostModal();
            watchSetupClose(kind, done);
            return;
        }
        switchAll(lst, on);
        if (!on) toast((kind === 'lora' ? (lst.length === 1 ? rowName(lst[0]) : 'Radios') : 'Hosting') + ' is off');
    }
    function switchAll(lst, on) {
        lst.forEach(function(c) {
            if (on && (c.status === 'off' || c.status === 'paused')) {
                RS.invoke('resume_interface', { args: { name: c.id, iface_type: c.ifaceType } }).catch(function(err) { fail(err, 'Could not turn it on'); });
            } else if (!on && c.status !== 'off') {
                RS.invoke('pause_interface', { args: { name: c.id, iface_type: c.ifaceType, off: true } }).catch(function(err) { fail(err, 'Could not turn it off'); });
            }
        });
    }
    function internetSwitch(on) {
        var lst = byKind(model(), 'tcp');
        if (!lst.length) { NV.internetOpen = on; render(true); return; }
        switchAll(lst, on);
        if (!on) toast('Internet is off');
    }
    // One action per way's switch, from the switch itself or a tap on the map.
    function switchWay(kind, on) {
        if (kind === 'tcp') { internetSwitch(on); return; }
        if (kind === 'ble') { if (on) enableBle(); else disableBle(); return; }
        if (kind === 'lora' || kind === 'host') { deviceSwitch(kind, on); return; }
        if (kind !== 'local') return;
        if (on) {
            var paused = byKind(model(), 'local')[0];
            if (paused && paused.status === 'paused') RS.invoke('resume_interface', { args: { name: paused.id, iface_type: paused.ifaceType } }).catch(function(err) { fail(err, 'Could not turn on Local Network'); });
            else if (typeof showAutoInterfaceConfigSheet === 'function') showAutoInterfaceConfigSheet();
        } else {
            RS.invoke('disable_auto_interface', {}).then(function() { toast('Local Network is off'); }).catch(function(err) { fail(err, 'Could not turn off Local Network'); });
        }
        render(true);
    }
    function foldWay(kind) {
        var i = NV.waysCollapsed.indexOf(kind);
        NV.waysCollapsed = i === -1 ? NV.waysCollapsed.concat(kind) : NV.waysCollapsed.filter(function(k) { return k !== kind; });
        RS.invoke('set_ui_state', { key: 'network_ways_collapsed', value: NV.waysCollapsed }).catch(function() {});
        render(true);
    }
    // Setup dialogs are app sheets; when the last one closes, the switch follows what was added.
    // Only a shown sheet counts: the More sheet's modal stays in the page, hidden.
    function watchSetupClose(kind, done) {
        var seen = false, tries = 0;
        var timer = setInterval(function() {
            var open = Array.prototype.some.call(document.querySelectorAll('.bottom-sheet.open, .rs-sheet-shell.open, .ui-modal'), function(el) {
                return !el.hidden && getComputedStyle(el).display !== 'none';
            });
            if (open) seen = true;
            if (NV.setupFor !== kind || (seen && !open) || ++tries > 600) { clearInterval(timer); done(); }
        }, 250);
    }
    // A running host's discovery switches save as they change. Turning on Show in discovery waits for the
    // address others use; Private network waits for a network identity.
    function hostDraft(c) {
        var saved = RS.hostDiscovery.state(c.iface), d = (NV.hostDrafts || {})[c.id];
        return d ? Object.assign({}, saved, d) : saved;
    }
    function hostReady(d) {
        var id = RS.networkIdentity && RS.networkIdentity.state();
        return !(d.disc && !d.reachable) && !(d.disc && d.priv && !(id && id.exists));
    }
    function saveHost(c, d) {
        var i = c.iface || {}, saved = RS.hostDiscovery.state(i), backbone = c.ifaceType === 'backbone_server';
        var args = { old_name: c.id, name: c.id, listen_port: Number(i.listen_port) || 4242,
            discoverable: !!d.disc, reachable_on: d.reachable || '', publish_ifac: !!(d.disc && d.share && d.ifac), discovery_encrypt: !!(d.disc && d.priv) };
        if (i.listen_ip) args.listen_ip = i.listen_ip;
        if (backbone) { args.prefer_ipv6 = typeof _ifaceBool === 'function' && _ifaceBool(i, 'prefer_ipv6', false); if (i.device) args.device = i.device; }
        var run = function() { return RS.invoke(backbone ? 'update_backbone_server' : 'update_tcp_server', { args: args }); };
        var settle = function() { delete NV.hostDrafts[c.id]; refresh(); render(true); };
        var word = args.discovery_encrypt ? 'Only your private network can find this host now.' : args.discoverable ? 'Anyone with discovery on can find this host now.' : 'This host no longer shows in discovery.';
        // Reticulum reads a server's discovery settings at start, so a published host restarts the network.
        if ((saved.disc || d.disc) && RS.networkSettings && typeof RS.networkSettings.commitRestart === 'function') {
            RS.networkSettings.commitRestart({ run: run, done: word }).then(settle);
            return;
        }
        run().then(function() { toast(word); settle(); }, function(err) { fail(err, 'Couldn’t change discovery'); settle(); });
    }
    function hostChange(el) {
        var box = el.closest('[data-host]'), c = box && byId(model(), box.getAttribute('data-host'));
        if (!c) return;
        NV.hostDrafts = NV.hostDrafts || {};
        var d = NV.hostDrafts[c.id] = hostDraft(c), key = el.getAttribute('data-hd');
        if (key === 'reachable') d.reachable = el.value.trim();
        else d[key] = el.checked;
        if (hostReady(d)) { saveHost(c, d); return; }
        render(true);
        var reach = document.getElementById('hc-reachable');
        if (key === 'disc' && d.disc && reach && !reach.value) reach.focus();
    }
    function flushHostDrafts() {
        var M = model();
        Object.keys(NV.hostDrafts || {}).forEach(function(id) {
            var c = byId(M, id), d = NV.hostDrafts[id];
            if (c && hostReady(d)) saveHost(c, d);
        });
    }
    function discEntry(id) { return (model().disc.entries || []).filter(function(e) { return e.id === id; })[0]; }
    function setMapCollapsed(collapsed) {
        NV.mapCollapsed = collapsed;
        RS.invoke('set_ui_state', { key: 'network_map_collapsed', value: collapsed }).catch(function() {});
        render(true);
        anim.ensure();
    }

    // A map node that is off leads to its way below: a quick scroll, a brief tint, then its switch turns on.
    function reveal(kind, keyboard) {
        var card = root && root.querySelector('.net-way.k-' + kind);
        if (!card) return;
        NV.flash = kind;
        card.classList.add('is-flash');
        clearTimeout(NV.flashTimer);
        scrollToWay(card, function() {
            var M = model();
            if (!(kind === 'ble' && (M.ble.on || M.ble.starting)) && !(kind === 'tcp' && NV.internetOpen)) switchWay(kind, true);
            var now = root.querySelector('.net-way.k-' + kind);
            if (keyboard && now) { var f = now.querySelector('input'); if (f) f.focus({ preventScroll: true }); }
            NV.flashTimer = setTimeout(function() {
                NV.flash = null;
                var el = root.querySelector('.net-way.k-' + kind);
                if (el) el.classList.remove('is-flash');
            }, 1100);
        });
    }
    function scrollToWay(card, done) {
        var sc = document.getElementById('netmap-scroll') || card.closest('.netmap-insp-scroll');
        if (!sc) { card.scrollIntoView({ block: 'nearest' }); done(); return; }
        var head = sc.querySelector('.netmap-insp-head'), above = head && getComputedStyle(head).position === 'sticky' ? head.offsetHeight : 0;
        var from = sc.scrollTop, max = sc.scrollHeight - sc.clientHeight;
        var to = Math.max(0, Math.min(max, from + card.getBoundingClientRect().top - sc.getBoundingClientRect().top - above - 12));
        var dist = Math.abs(to - from);
        if (dist < 2 || anim.reduced()) { sc.scrollTop = to; done(); return; }
        var ms = Math.min(380, Math.max(220, dist * 0.45)), t0 = 0, over = false;
        // Frames stop in a hidden WebView; the timer still lands the scroll and the switch.
        var finish = function() { if (over) return; over = true; sc.scrollTop = to; done(); };
        var guard = setTimeout(finish, ms + 120);
        requestAnimationFrame(function step(t) {
            if (over) return;
            if (!t0) t0 = t;
            var k = Math.min(1, (t - t0) / ms);
            sc.scrollTop = from + (to - from) * (1 - Math.pow(1 - k, 3));
            if (k < 1) requestAnimationFrame(step); else { clearTimeout(guard); finish(); }
        });
    }

    function onClick(e) {
        if (e.target.closest && e.target.closest('.net-hostdisc [data-netid]') && RS.hostDiscovery.handleInline(e)) return;
        // A way that folds folds from anywhere on its head but the switch.
        var head = e.target.closest && e.target.closest('.net-way-head');
        if (head && !e.target.closest('.ui-switch, button, input, a')) {
            var fold = head.querySelector('[data-act="way-fold"]');
            if (fold) { foldWay(fold.getAttribute('data-id')); return; }
        }
        var lbl = e.target.closest && e.target.closest('.netmap-label');
        var el = lbl ? lbl.parentNode.querySelector('.netmap-node-btn') : e.target.closest && e.target.closest('[data-act]');
        if (!el || el.tagName === 'INPUT') return;
        var a = el.getAttribute('data-act'), id = el.getAttribute('data-id');
        switch (a) {
            case 'nav-activity': if (typeof switchView === 'function') switchView('activity'); return;
            case 'announce':
                if (el.disabled || NV.announcing || typeof tryTriggerAnnounce !== 'function' || !tryTriggerAnnounce()) return;
                NV.announcing = true; NV.lastAnnounce = Date.now(); render(true); anim.announce();
                setTimeout(function() { NV.announcing = false; render(true); }, 2500);
                return;
            case 'map-toggle': setMapCollapsed(!NV.mapCollapsed); return;
            case 'way-fold': foldWay(id); return;
            case 'add-radio': if (typeof openRnodeModal === 'function') openRnodeModal('ble'); return;
            case 'pause-all': pauseAll(); return;
            case 'resume-all': resumeAll(); return;
            case 'join': {
                var s = (typeof PUBLIC_TCP_SERVERS !== 'undefined' ? PUBLIC_TCP_SERVERS : []).filter(function(x) { return x.id === id; })[0];
                if (!s) return;
                if (typeof joinPublicServer === 'function') joinPublicServer(s, serverState(s).match);
                render(true);
                return;
            }
            case 'more-servers': NV.moreOpen = !NV.moreOpen; render(true); return;
            case 'join-toggle': NV.joinOpen = !NV.joinOpen; render(true); return;
            case 'own': if (typeof openConnectModal === 'function') openConnectModal(); return;
            case 'ble-dur': setBleDuration(parseInt(el.getAttribute('data-v'), 10) || 0, el.textContent); return;
            case 'c-node': openConn(id); return;
            case 'c-ghost': reveal(id, e.detail === 0); return;
            case 'c-menu': {
                var M = model(), c = byId(M, id);
                if (c && RS.kit) RS.kit.menu(el, { title: rowName(c), items: menuItems(c).map(function(x) {
                    if (x.separator) return x;
                    return { label: x.label, icon: x.icon, danger: x.danger, onSelect: function() { connOp(x.op, id, el); } };
                }) });
                return;
            }
            case 'op': connOp(el.getAttribute('data-op'), id, el); if (RS.kit && RS.kit.closeMenu) RS.kit.closeMenu(); return;
            case 'back': NV.sel = null; NV.view = null; render(true); return;
            case 'back-list': NV.sel = null; NV.view = 'list'; render(true); return;
            case 'list': openList(); return;
            case 'd-sel': openDisc(id, true); return;
            case 'dfilter': NV.dfilter = el.getAttribute('data-v') || 'all'; render(true); if (NV.listSheet) NV.listSheet.body.innerHTML = discList(model()); return;
            case 'disc-older': NV.older = true; render(true); if (NV.listSheet) NV.listSheet.body.innerHTML = discList(model()); return;
            case 'disc-settings':
                if (typeof switchView === 'function') switchView('settings');
                if (typeof selectSettingsSection === 'function') selectSettingsSection('panel-settings-network', { showMobileDetail: compact() });
                return;
            case 'disc-join': { var ej = discEntry(id); if (ej && RS.discovery) RS.discovery.connect(ej); return; }
            case 'disc-keep': { var ek = discEntry(id); if (ek && RS.discovery) RS.discovery.keep(ek.joined_interface); return; }
            case 'disc-radio': { var er = discEntry(id); if (er && RS.discovery) RS.discovery.useRadio(er); return; }
            case 'disc-trust': { var et = discEntry(id); if (et && RS.discovery) RS.discovery.trust(et); return; }
        }
    }
    function onChange(e) {
        var hd = e.target.closest && e.target.closest('.net-hostdisc [data-hd]');
        if (hd) { hostChange(hd); return; }
        var el = e.target.closest && e.target.closest('input[data-act]');
        if (!el) return;
        var a = el.getAttribute('data-act');
        if (a === 'tcp' || a === 'ble' || a === 'local') { switchWay(a, el.checked); return; }
        if (a === 'dev-lora' || a === 'dev-host') { switchWay(a.slice(4), el.checked); return; }
        if (a === 'disc') {
            var input = document.getElementById('ns-discover-toggle');
            if (input) { input.checked = el.checked; input.dispatchEvent(new Event('change')); }
        }
    }

    function init() {
        root = document.getElementById('network-root');
        if (!root) return;
        root.addEventListener('click', onClick);
        root.addEventListener('change', onChange);
        if (window.ResizeObserver) {
            mapObs = new ResizeObserver(function() {
                var map = document.getElementById('netmap-map');
                if (map && (map.clientWidth !== +map.getAttribute('data-w') || map.clientHeight !== +map.getAttribute('data-h'))) layoutMap(model());
            });
        }
        if (RS.uiStateListeners) RS.uiStateListeners.push(function(data) {
            var before = [NV.mapCollapsed, NV.waysCollapsed.join(), NV.connectedOnce].join('|');
            if (data.network_map_collapsed !== undefined) NV.mapCollapsed = !!data.network_map_collapsed;
            if (Array.isArray(data.network_ways_collapsed)) NV.waysCollapsed = data.network_ways_collapsed.slice();
            if (data.network_connected_once === true) NV.connectedOnce = true;
            if ([NV.mapCollapsed, NV.waysCollapsed.join(), NV.connectedOnce].join('|') !== before) NV.render(true);
        });
        if (typeof watchViewLayout === 'function') watchViewLayout('network', function() { NV.render(true); });
        if (RS.networkIdentity) RS.networkIdentity.onChange(function() { flushHostDrafts(); NV.render(true); });
        window.addEventListener('ratspeak-theme-changed', function() { NV.render(true); });
        if (document.fonts && document.fonts.ready) document.fonts.ready.then(sizeTotals);
        // Bluetooth Peer's time left counts down on the card while Network shows.
        setInterval(function() { if (window._blePeerEnabled && document.visibilityState === 'visible') NV.render(); }, 30000);
    }
    if (typeof VIEW_LIFECYCLE !== 'undefined') {
        var previous = VIEW_LIFECYCLE.network;
        VIEW_LIFECYCLE.network = function(dirty) {
            if (typeof previous === 'function') previous(dirty);
            NV.render(true);
            anim.ensure();
        };
    }
    if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', init);
    else init();
})();
