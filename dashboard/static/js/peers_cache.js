// PeersCache: single source of truth for peer rows on the JS side.
// Loaded once from `api_get_peers_snapshot`, updated by `peer_updated` /
// `peer_removed` events. Status tier is computed from `last_seen` alone —
// path_table presence informs route info but never the status dot.
// Tiers: reachable <2h, stale <1d, offline <7d, then culled. `unreachable`
// is a contact-only sentinel for rows where last_seen is null.
var PeersCache = (function() {
    var STALE_AFTER_SECS = 2 * 60 * 60;
    var OFFLINE_AFTER_SECS = 24 * 60 * 60;
    var CULL_AFTER_SECS = 7 * 24 * 60 * 60;
    var RETIER_INTERVAL_MS = 60 * 1000;

    var _cache = Object.create(null); // hash → entry
    var _subs = [];
    var _initialized = false;
    var _initPromise = null;
    var _retierTimer = null;

    // Memoized enriched() output; invalidated on cache mutation or when the
    // path_table reference changes. Avoids O(n) realloc on every render.
    var _enrichedCache = null;
    var _enrichedPathTable = null;
    var _enrichedPathIndex = null;

    function _isSuppressedPeerDisplayName(displayName) {
        if (typeof displayName !== 'string') return false;
        var name = displayName.trim();
        if (!name) return false;
        return /meshtastic/i.test(name) || /^![a-f0-9]{8}$/i.test(name);
    }

    function _normalizeServices(services) {
        if (!Array.isArray(services)) return [];
        var out = [];
        for (var i = 0; i < services.length; i++) {
            if (typeof services[i] !== 'string') continue;
            var s = services[i].trim();
            if (s && out.indexOf(s) === -1) out.push(s);
        }
        return out;
    }

    function _hasSupportedPeerService(entry) {
        if (!entry) return false;
        if (entry.is_contact) return true;
        var services = Array.isArray(entry.services) ? entry.services : [];
        return services.indexOf('lxmf.delivery') !== -1 || services.indexOf('lxst.telephony') !== -1;
    }

    function _supportsRatspeakFeatures(entry) {
        if (!entry) return false;
        var services = Array.isArray(entry.services) ? entry.services : [];
        return services.indexOf('ratspeak.client') !== -1;
    }

    function _isSuppressedPeerEntry(entry) {
        return !!entry && (
            _isSuppressedPeerDisplayName(entry.display_name) ||
            !_hasSupportedPeerService(entry)
        );
    }

    function _notify() {
        _enrichedCache = null;
        for (var i = 0; i < _subs.length; i++) {
            try { _subs[i](); } catch (e) { /* swallow */ }
        }
    }

    function _normalizeRow(r) {
        if (!r || !r.hash) return null;
        var services = _normalizeServices(r.services);
        return {
            hash: r.hash,
            identity_hash: typeof r.identity_hash === 'string' ? r.identity_hash : '',
            telephony_hash: typeof r.telephony_hash === 'string' ? r.telephony_hash : '',
            last_seen: (r.last_seen === undefined || r.last_seen === null) ? null : r.last_seen,
            first_seen: (r.first_seen === undefined || r.first_seen === null) ? null : r.first_seen,
            display_name: typeof r.display_name === 'string' ? r.display_name : '',
            is_contact: !!r.is_contact,
            last_interface: typeof r.last_interface === 'string' ? r.last_interface : '',
            services: services,
            supports_ratspeak: services.indexOf('ratspeak.client') !== -1,
        };
    }

    // Idempotent — concurrent callers share the same in-flight Promise.
    function init() {
        if (_initPromise) return _initPromise;
        if (typeof RS === 'undefined' || typeof RS.invoke !== 'function') {
            return Promise.reject(new Error('RS.invoke unavailable'));
        }
        _initPromise = RS.invoke('api_get_peers_snapshot')
            .then(function(rows) {
                var fresh = Object.create(null);
                if (Array.isArray(rows)) {
                    for (var i = 0; i < rows.length; i++) {
                        var n = _normalizeRow(rows[i]);
                        if (n) fresh[n.hash] = n;
                    }
                }
                _cache = fresh;
                _initialized = true;
                if (!_retierTimer) {
                    _retierTimer = setInterval(_notify, RETIER_INTERVAL_MS);
                }
                _notify();
            })
            .catch(function(e) {
                window.RS.diag('error', '[PeersCache] init failed:', e);
                _initPromise = null; // allow retry
                throw e;
            });
        return _initPromise;
    }

    function _applyOne(payload) {
        var n = _normalizeRow(payload);
        if (!n) return false;
        var existing = _cache[n.hash];
        if (existing) {
            // last_interface is sticky (empty-string delta preserves the
            // prior value); first_seen is write-once (null preserves).
            if (payload.last_seen !== undefined) existing.last_seen = n.last_seen;
            if (payload.first_seen !== undefined && n.first_seen != null) {
                existing.first_seen = n.first_seen;
            }
            if (payload.display_name !== undefined) existing.display_name = n.display_name;
            if (payload.is_contact !== undefined) existing.is_contact = n.is_contact;
            if (payload.last_interface !== undefined && n.last_interface) {
                existing.last_interface = n.last_interface;
            }
            if (payload.identity_hash !== undefined) existing.identity_hash = n.identity_hash;
            if (payload.telephony_hash !== undefined) existing.telephony_hash = n.telephony_hash;
            if (payload.services !== undefined) existing.services = n.services;
        } else {
            _cache[n.hash] = n;
        }
        return true;
    }

    function applyUpdated(payload) {
        if (_applyOne(payload)) _notify();
    }

    // Single notify per batch — N separate emits drained the wry/JNI
    // global-ref pool on busy hubs and SIGABRT'd within ~10 minutes.
    function applyBatch(rows) {
        if (!Array.isArray(rows) || rows.length === 0) return;
        var any = false;
        for (var i = 0; i < rows.length; i++) {
            if (_applyOne(rows[i])) any = true;
        }
        if (any) _notify();
    }

    function applyRemoved(hash) {
        if (!hash) return;
        if (_cache[hash]) {
            delete _cache[hash];
            _notify();
        }
    }

    // Called on identity / factory reset so the pre-reset list doesn't
    // briefly paint while the page reload is in flight.
    function clear() {
        _cache = Object.create(null);
        _initialized = false;
        _initPromise = null;
        _notify();
    }

    function get(hash) {
        var entry = _cache[hash] || null;
        return _isSuppressedPeerEntry(entry) ? null : entry;
    }

    function getAll() {
        var out = [];
        for (var h in _cache) {
            if (!Object.prototype.hasOwnProperty.call(_cache, h)) continue;
            if (_isSuppressedPeerEntry(_cache[h])) continue;
            out.push(_cache[h]);
        }
        return out;
    }

    function size() {
        var n = 0;
        for (var h in _cache) {
            if (!Object.prototype.hasOwnProperty.call(_cache, h)) continue;
            if (_isSuppressedPeerEntry(_cache[h])) continue;
            n++;
        }
        return n;
    }

    function subscribe(fn) {
        if (typeof fn !== 'function') return function() {};
        _subs.push(fn);
        return function() {
            var i = _subs.indexOf(fn);
            if (i >= 0) _subs.splice(i, 1);
        };
    }

    // Pure function of last_seen — interface add/remove never moves tiers.
    function computeStatus(entry, nowSecs) {
        if (entry.last_seen == null) {
            return 'unreachable';
        }
        var age = nowSecs - entry.last_seen;
        if (age < STALE_AFTER_SECS) return 'reachable';
        if (age < OFFLINE_AFTER_SECS) return 'stale';
        if (age < CULL_AFTER_SECS) return 'offline';
        return 'offline';
    }

    function computeActivity(entry, nowSecs) {
        if (entry.last_seen == null) return { tier: 'never', label: 'Never seen' };
        var age = Math.max(0, nowSecs - entry.last_seen);
        if (age < STALE_AFTER_SECS) return { tier: 'recent', label: 'Last heard recently' };
        if (age < OFFLINE_AFTER_SECS) return { tier: 'today', label: 'Last heard today' };
        return { tier: 'older', label: 'Last heard ' + prettyTime(age) + ' ago' };
    }

    function computeRoute(pi) {
        if (!pi) return { state: 'none', label: 'No current path' };
        var hops = pi.hops != null ? pi.hops : null;
        if (hops === 0) return { state: 'direct', label: 'Direct' };
        if (hops !== null) return { state: 'routed', label: hops + ' hop' + (hops !== 1 ? 's' : '') };
        return { state: 'available', label: 'Available' };
    }

    function pathInfo(hash, service, pathLookup, nowSecs) {
        var pi = hash ? (pathLookup[hash] || null) : null;
        var route = computeRoute(pi);
        return {
            hash: hash || '',
            service: service,
            path: pi,
            hops: pi ? (pi.hops != null ? pi.hops : null) : null,
            via: pi ? (pi.via || null) : null,
            iface: pi ? (pi.interface || null) : null,
            path_age: (pi && pi.timestamp) ? (nowSecs - pi.timestamp) : null,
            in_path: !!pi,
            route_state: route.state,
            route_label: route.label,
        };
    }

    function primaryRouteInfo(messageInfo, voiceInfo) {
        if (messageInfo.in_path) return messageInfo;
        if (voiceInfo.in_path) return voiceInfo;
        return messageInfo;
    }

    function isInitialized() { return _initialized; }

    // Cache rows overlaid with the live path index. `path_table` is capped for
    // render cost; `path_index` stays compact but covers the full route set.
    // Same row shape as connections.js/lxmf.js/health.js expect.
    function enriched() {
        var pathIndex = (typeof lastStats !== 'undefined' && lastStats && lastStats.path_index && typeof lastStats.path_index === 'object')
            ? lastStats.path_index
            : null;
        var pathTable = (typeof lastStats !== 'undefined' && lastStats && Array.isArray(lastStats.path_table))
            ? lastStats.path_table
            : null;
        if (_enrichedCache && _enrichedPathTable === pathTable && _enrichedPathIndex === pathIndex) {
            return _enrichedCache;
        }
        var entries = getAll();
        var nowSecs = Date.now() / 1000;
        var pathLookup = Object.create(null);
        if (pathIndex) {
            for (var h in pathIndex) {
                if (Object.prototype.hasOwnProperty.call(pathIndex, h)) pathLookup[h] = pathIndex[h];
            }
        } else if (pathTable) {
            for (var i = 0; i < pathTable.length; i++) {
                var p = pathTable[i];
                if (p && p.hash) pathLookup[p.hash] = p;
            }
        }
        var out = new Array(entries.length);
        for (var j = 0; j < entries.length; j++) {
            var entry = entries[j];
            var messageInfo = pathInfo(entry.hash, 'lxmf.delivery', pathLookup, nowSecs);
            var voiceInfo = pathInfo(entry.telephony_hash || '', 'lxst.telephony', pathLookup, nowSecs);
            var primaryInfo = primaryRouteInfo(messageInfo, voiceInfo);
            var pi = primaryInfo.path;
            var hops = primaryInfo.hops;
            var pathAge = primaryInfo.path_age;
            var activity = computeActivity(entry, nowSecs);
            var routeLabel = primaryInfo.route_label;
            if (primaryInfo.service === 'lxst.telephony' && primaryInfo.in_path) {
                routeLabel = 'Voice: ' + routeLabel;
            }
            // Iface precedence: live path_table > persisted last_interface
            // (so a peer survives a reboot with a route badge).
            var liveIface = pi ? (pi.interface || null) : null;
            var iface = liveIface || (entry.last_interface || null);
            out[j] = {
                hash: entry.hash,
                identity_hash: entry.identity_hash || '',
                telephony_hash: entry.telephony_hash || '',
                display_name: entry.display_name || '',
                is_contact: !!entry.is_contact,
                services: Array.isArray(entry.services) ? entry.services.slice() : [],
                supports_ratspeak: _supportsRatspeakFeatures(entry),
                supports_lxst_call: Array.isArray(entry.services) && entry.services.indexOf('lxst.telephony') !== -1,
                last_seen: entry.last_seen,
                first_seen: entry.first_seen,
                last_interface: entry.last_interface || '',
                hops: hops,
                via: pi ? (pi.via || null) : null,
                iface: iface,
                iface_is_live: !!liveIface,
                path_age: pathAge,
                in_path: !!pi,
                route_hash: primaryInfo.hash,
                route_service: primaryInfo.service,
                message_route_hash: messageInfo.hash,
                message_route_label: messageInfo.route_label,
                message_route_state: messageInfo.route_state,
                message_in_path: messageInfo.in_path,
                message_hops: messageInfo.hops,
                message_path_age: messageInfo.path_age,
                message_iface: messageInfo.iface,
                message_iface_is_live: messageInfo.in_path,
                voice_route_hash: voiceInfo.hash,
                voice_route_label: voiceInfo.route_label,
                voice_route_state: voiceInfo.route_state,
                voice_in_path: voiceInfo.in_path,
                voice_hops: voiceInfo.hops,
                voice_path_age: voiceInfo.path_age,
                voice_iface: voiceInfo.iface,
                voice_iface_is_live: voiceInfo.in_path,
                status: computeStatus(entry, nowSecs),
                activity_tier: activity.tier,
                activity_label: activity.label,
                route_state: primaryInfo.route_state,
                route_label: routeLabel,
            };
        }
        _enrichedCache = out;
        _enrichedPathTable = pathTable;
        _enrichedPathIndex = pathIndex;
        return out;
    }

    return {
        init: init,
        applyUpdated: applyUpdated,
        applyBatch: applyBatch,
        applyRemoved: applyRemoved,
        clear: clear,
        get: get,
        getAll: getAll,
        size: size,
        subscribe: subscribe,
        computeStatus: computeStatus,
        supportsRatspeakFeatures: _supportsRatspeakFeatures,
        isSuppressedPeerDisplayName: _isSuppressedPeerDisplayName,
        enriched: enriched,
        isInitialized: isInitialized,
    };
})();
