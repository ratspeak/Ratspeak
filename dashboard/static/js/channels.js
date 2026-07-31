// Channels: live group conversations with a bounded, identity-scoped timeline.
// Accepted room activity is persisted only by the native database API: never
// in browser storage, never as LXMF messages, and never as a hub backlog.

var channelsSnapshot = {
    protocol_version: '0.1.3',
    service_model_version: 3,
    generation: 0,
    revision: 0,
    connection_budget: 1,
    selected_hub_destination: null,
    hubs: [],
    durability: {
        phase: 'loading',
        last_error: null
    },
    history: {
        phase: 'unavailable',
        pending_events: 0,
        dropped_events: 0,
        last_error: null
    },
    phase: 'unavailable',
    nickname: null,
    hub: null,
    rooms: [],
    directory: {
        phase: 'idle',
        rooms: [],
        complete: false,
        omitted_count: 0,
        refreshed_at_ms: null,
        last_error: null
    },
    hub_greeting: null,
    notices: [],
    last_error: null,
    updated_at_ms: 0
};
var channelsDiscoveredHubs = [];
var channelsSavedHubs = [];
var channelsSavedRooms = [];
var channelsRoomIndex = [];
var channelsUnread = {
    rooms: [],
    unread_total: 0,
    mention_total: 0,
    attention_total: 0
};
var channelsActiveRoom = null;
var channelsHistorySelection = null;
var channelsPendingShareJoin = null;
var channelsPendingHubLabel = '';
var _channelsLoadedAt = 0;
var _channelsLastHubRefreshAt = 0;
var _channelsLoadPromise = null;
var _channelsHubRefreshPromise = null;
var _channelsDirectoryRefreshPromise = null;
var _channelsDirectoryRequestSeq = 0;
var _channelsDiscoveryRefreshTimer = null;
var _channelsSavedRoomsHub = null;
var _channelsSaveHubKey = null;
var _channelsSaveHubPromise = null;
var _channelsSavedRoomKeys = {};
var _channelsSendPending = false;
var _channelsFieldSeq = 0;
var _channelsLocalRoomEvents = {};
var _channelsLocalEventSeq = 0;
var _channelsExpandedPresenceGroups = {};
var _channelsPresenceGroupSeq = 0;
var _channelsSelectedMemberKey = null;
var _channelsMemberReturnFocusKey = null;
var _channelsHistoryCache = {};
var _channelsHistoryRequestSeq = 0;
var _channelsHistoryEpoch = 0;
var _channelsRoomIndexRequestSeq = 0;
var _channelsUnreadRequestSeq = 0;
var _channelsRenderedRoomKey = '';
var _channelsHubSwitcherDismiss = null;
var CHANNEL_PRESENCE_GROUP_WINDOW_MS = 5 * 60 * 1000;
// Brief leave/rejoin churn is one continuous presence when nothing happens between it.
var CHANNEL_PRESENCE_REJOIN_WINDOW_MS = 15 * 1000;
var CHANNEL_HISTORY_PAGE_SIZE = 100;
var CHANNEL_HISTORY_SYNC_PAGE_SIZE = 200;
var CHANNEL_HISTORY_CACHE_ROOM_LIMIT = 5000;
var CHANNEL_HISTORY_MAX_SYNC_PAGES = 32;
var CHANNEL_DIRECTORY_STALE_AFTER_MS = 5 * 60 * 1000;

function _channelsEl(id) {
    return document.getElementById(id);
}

function _channelsCompact() {
    return !!(window.matchMedia && window.matchMedia('(max-width: 768px)').matches);
}

function _channelsIsConnected() {
    return channelsSnapshot.phase === 'active' || channelsSnapshot.phase === 'stale';
}

function _channelsIsConnecting() {
    return channelsSnapshot.phase === 'resolving' ||
        channelsSnapshot.phase === 'connecting' ||
        channelsSnapshot.phase === 'awaiting_welcome' ||
        channelsSnapshot.phase === 'reconnecting';
}

function _channelsViewVisible() {
    var view = _channelsEl('view-channels');
    return !!(view && view.classList && view.classList.contains('active'));
}

function _channelsDirectoryNeedsRefresh() {
    if (!_channelsIsConnected()) return false;
    var directory = channelsSnapshot.directory || {};
    if (directory.phase === 'idle') return true;
    if (directory.phase !== 'ready') return false;
    var refreshedAt = Number(directory.refreshed_at_ms);
    return !Number.isFinite(refreshedAt) ||
        Date.now() - refreshedAt >= CHANNEL_DIRECTORY_STALE_AFTER_MS;
}

function _channelsShortHash(value) {
    var text = String(value || '');
    if (typeof shortHash === 'function') return shortHash(text, 8, 4);
    return text.length > 14 ? text.slice(0, 8) + '\u2026' + text.slice(-4) : text;
}

function _channelsUtf8Length(value) {
    var text = String(value || '');
    if (window.TextEncoder) return new TextEncoder().encode(text).length;
    return unescape(encodeURIComponent(text)).length;
}

function _channelsUtf8Truncate(value, maxBytes) {
    var result = '';
    var used = 0;
    Array.from(String(value || '')).some(function(character) {
        var bytes = _channelsUtf8Length(character);
        if (used + bytes > maxBytes) return true;
        result += character;
        used += bytes;
        return false;
    });
    return result;
}

function _channelsMessageBody(value) {
    var text = String(value || '');
    return text.indexOf('/me ') === 0 ? text.slice(4) : text;
}

function _channelsMessageLimit() {
    var limits = channelsSnapshot.hub && channelsSnapshot.hub.limits;
    return (limits && limits.max_message_body_bytes) || 350;
}

function _channelsCanCompose() {
    var room = channelsActiveRoom ? _channelsRoomByName(channelsActiveRoom) : null;
    return !!room && !channelsHistorySelection && room.phase === 'joined' &&
        channelsSnapshot.phase === 'active';
}

function _channelsInsertComposerText(value) {
    var input = _channelsEl('channel-message-input');
    if (!input || !_channelsCanCompose()) return false;
    var insertion = String(value || '');
    if (!insertion) return false;
    var current = String(input.value || '');
    var start = Number.isInteger(input.selectionStart) ? input.selectionStart : current.length;
    var end = Number.isInteger(input.selectionEnd) ? input.selectionEnd : start;
    start = Math.max(0, Math.min(start, current.length));
    end = Math.max(start, Math.min(end, current.length));
    var before = current.slice(0, start);
    var after = current.slice(end);
    var characters = Array.from(insertion);
    var fitted = characters.join('');
    var limit = _channelsMessageLimit();
    while (characters.length &&
            _channelsUtf8Length(_channelsMessageBody(before + fitted + after)) > limit) {
        characters.pop();
        fitted = characters.join('');
    }
    if (!fitted ||
            _channelsUtf8Length(_channelsMessageBody(before + fitted + after)) > limit) {
        if (typeof showToast === 'function') {
            showToast('The channel message is already at its byte limit', 'toast-orange', 2400);
        }
        return false;
    }
    input.value = before + fitted + after;
    var cursor = before.length + fitted.length;
    if (typeof input.setSelectionRange === 'function') {
        input.setSelectionRange(cursor, cursor);
    }
    input.style.height = 'auto';
    input.style.height = Math.min(input.scrollHeight, 132) + 'px';
    _channelsUpdateComposer();
    input.focus();
    return true;
}

function _channelsInsertQuote(item, authorText) {
    var input = _channelsEl('channel-message-input');
    if (!input || !item) return false;
    var body = String(item.text || '').replace(/\s+/g, ' ').trim();
    if (!body) return false;
    var author = String(authorText || 'Channel member').replace(/\s+/g, ' ').trim();
    var quote = item.kind === 'action'
        ? '> * ' + author + ' ' + body
        : '> ' + author + ': ' + body;
    quote = _channelsUtf8Truncate(quote, Math.min(200, _channelsMessageLimit()));
    var start = Number.isInteger(input.selectionStart) ? input.selectionStart : input.value.length;
    var needsLineBreak = start > 0 && input.value.charAt(start - 1) !== '\n';
    return _channelsInsertComposerText((needsLineBreak ? '\n' : '') + quote + '\n\n');
}

function _channelsInsertMemberMention(member) {
    if (!member || member.is_self) return false;
    var target = String(member.nickname || member.identity_hash || '').trim();
    if (!target || /[\u0000-\u001f\u007f]/.test(target)) return false;
    var input = _channelsEl('channel-message-input');
    if (!input) return false;
    var start = Number.isInteger(input.selectionStart) ? input.selectionStart : input.value.length;
    var previous = start > 0 ? input.value.charAt(start - 1) : '';
    var leading = previous && !/\s/.test(previous) ? ' ' : '';
    return _channelsInsertComposerText(leading + '@' + target + ' ');
}

function _channelsDurableRoom(roomName) {
    var destination = channelsSnapshot.hub && channelsSnapshot.hub.destination_hash;
    var hubs = Array.isArray(channelsSnapshot.hubs) ? channelsSnapshot.hubs : [];
    for (var i = 0; i < hubs.length; i++) {
        if (hubs[i].destination_hash !== destination) continue;
        var rooms = hubs[i].durable && Array.isArray(hubs[i].durable.rooms)
            ? hubs[i].durable.rooms : [];
        for (var j = 0; j < rooms.length; j++) {
            if (rooms[j].name === roomName) return rooms[j];
        }
    }
    return null;
}

function _channelsIdentityTone(value) {
    var text = String(value || 'channel-member');
    var hash = 2166136261;
    for (var i = 0; i < text.length; i++) {
        hash ^= text.charCodeAt(i);
        hash = Math.imul(hash, 16777619);
    }
    return String((hash >>> 0) % 6);
}

function _channelsFormatTime(timestampMs) {
    var date = new Date(Number(timestampMs) || Date.now());
    return date.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' });
}

function _channelsDefaultNickname() {
    // The live identity wins: the cached copy is a pre-load bootstrap hint and
    // goes stale on rename, which would broadcast a superseded name to the hub.
    var name = '';
    if (typeof activeIdentity === 'function') {
        var active = activeIdentity();
        if (active) name = active.display_name || active.nickname || '';
    }
    if (!name) {
        try { name = localStorage.getItem('ratspeak_identity_name') || ''; } catch (_) {}
    }
    name = String(name || 'rat').trim();
    return name.slice(0, 32) || 'rat';
}

function _channelsHubName(hub) {
    hub = hub || {};
    return hub.name || hub.announced_name || hub.label || 'Channel hub';
}

function _channelsTimelineHubName(hub) {
    var name = _channelsHubName(hub);
    if (name !== 'Channel hub' || !hub || !hub.destination_hash) return name;
    return 'Hub ' + _channelsShortHash(hub.destination_hash);
}

function _channelsOwnedHubReady() {
    return typeof channelHubOverview !== 'undefined' && channelHubOverview &&
        channelHubOverview.status && channelHubOverview.status.running &&
        typeof channelHubOwnDestinationHash === 'function' &&
        !!channelHubOwnDestinationHash();
}

function channelsConnectToHub(hub, options) {
    hub = hub || {};
    options = options || {};
    if (!options.preserve_pending_share) channelsPendingShareJoin = null;
    var destination = String(hub.destination_hash || '').trim().toLowerCase();
    var nickname = String(hub.nickname || _channelsDefaultNickname()).trim();
    if (!/^[0-9a-f]{32}$/.test(destination)) {
        return Promise.reject(new Error('Enter a 32-character hexadecimal destination hash.'));
    }
    if (!nickname) return Promise.reject(new Error('Choose a nickname for this session.'));
    channelsPendingHubLabel = hub.label || hub.announced_name || '';
    return RS.invoke('connect_channel_hub', {
        args: { destination_hash: destination, nickname: nickname }
    }).then(function(snapshot) {
        channelsActiveRoom = null;
        channelsHistorySelection = null;
        channelsApplySnapshot(snapshot);
        if (typeof showToast === 'function') {
            showToast(
                options.switching
                    ? 'Switching channel hub\u2026'
                    : 'Connecting to channel hub\u2026',
                'toast-blue',
                2600
            );
        }
        return snapshot;
    });
}

function _channelsPhaseLabel(phase) {
    switch (phase) {
        case 'resolving': return 'Finding path';
        case 'connecting': return 'Securing link';
        case 'awaiting_welcome': return 'Waiting for hub';
        case 'reconnecting': return 'Reconnecting';
        case 'active': return 'Connected';
        case 'stale': return 'Recovering';
        case 'error': return 'Session ended';
        case 'offline': return 'Not connected';
        default: return 'Unavailable';
    }
}

function _channelsRoomPhaseLabel(phase) {
    switch (phase) {
        case 'joining': return 'Joining…';
        case 'joined': return 'Live';
        case 'parting': return 'Leaving';
        case 'error': return 'Not joined';
        case 'history': return 'Local history';
        default: return 'Not joined';
    }
}

function _channelsRoomModeLabels(value) {
    var modes = String(value || '');
    var enabled = true;
    var active = {};
    for (var i = 0; i < modes.length; i++) {
        if (modes[i] === '+') enabled = true;
        else if (modes[i] === '-') enabled = false;
        else active[modes[i]] = enabled;
    }
    var labels = [];
    if (active.i) labels.push('Invite only');
    if (active.k) labels.push('Key required');
    if (active.m) labels.push('Moderated');
    if (active.n) labels.push('Members can post');
    if (active.p) labels.push('Private');
    if (active.t) labels.push('Topic managed by operators');
    return labels;
}

function _channelsRoomByName(name) {
    var rooms = Array.isArray(channelsSnapshot.rooms) ? channelsSnapshot.rooms : [];
    for (var i = 0; i < rooms.length; i++) {
        if (rooms[i].name === name) return rooms[i];
    }
    return null;
}

function _channelsHistoryKey(hubDestinationHash, roomName) {
    var hub = String(hubDestinationHash || '').trim().toLowerCase();
    var room = String(roomName || '').trim().toLowerCase();
    return hub && room ? hub + '\n' + room : '';
}

function _channelsUnreadCount(value) {
    value = Number(value);
    return Number.isSafeInteger(value) && value > 0 ? value : 0;
}

function channelsApplyUnread(summary) {
    summary = summary && typeof summary === 'object' ? summary : {};
    var rooms = Array.isArray(summary.rooms) ? summary.rooms : [];
    channelsUnread = {
        rooms: rooms.filter(function(room) {
            return room && _channelsHistoryKey(
                room.hub_destination_hash,
                room.room_name
            );
        }).map(function(room) {
            var level = ['all', 'mentions', 'mute'].indexOf(room.notification_level) !== -1
                ? room.notification_level : 'mentions';
            return {
                hub_destination_hash: String(room.hub_destination_hash).toLowerCase(),
                room_name: String(room.room_name).toLowerCase(),
                unread_count: _channelsUnreadCount(room.unread_count),
                mention_count: _channelsUnreadCount(room.mention_count),
                notification_level: level
            };
        }),
        unread_total: _channelsUnreadCount(summary.unread_total),
        mention_total: _channelsUnreadCount(summary.mention_total),
        attention_total: _channelsUnreadCount(summary.attention_total)
    };
    if (typeof setMessageUnreadSource === 'function') {
        setMessageUnreadSource('channels', channelsUnread.attention_total);
    }
    if (_channelsEl('channels-list')) _channelsRenderList();
    return channelsUnread;
}

function channelsRefreshUnread() {
    var request = ++_channelsUnreadRequestSeq;
    var epoch = _channelsHistoryEpoch;
    return RS.invoke('api_channel_unread').then(function(summary) {
        if (request !== _channelsUnreadRequestSeq || epoch !== _channelsHistoryEpoch) {
            return channelsUnread;
        }
        return channelsApplyUnread(summary);
    }).catch(function() {
        return channelsUnread;
    });
}

function _channelsRoomUnreadState(hubDestinationHash, roomName) {
    var key = _channelsHistoryKey(hubDestinationHash, roomName);
    for (var i = 0; i < channelsUnread.rooms.length; i++) {
        var room = channelsUnread.rooms[i];
        if (_channelsHistoryKey(room.hub_destination_hash, room.room_name) === key) {
            return room;
        }
    }
    return {
        hub_destination_hash: String(hubDestinationHash || '').toLowerCase(),
        room_name: String(roomName || '').toLowerCase(),
        unread_count: 0,
        mention_count: 0,
        notification_level: 'mentions'
    };
}

function _channelsHubByDestination(destinationHash) {
    var target = String(destinationHash || '').toLowerCase();
    var activeHub = channelsSnapshot.hub;
    if (activeHub && String(activeHub.destination_hash || '').toLowerCase() === target) {
        return activeHub;
    }
    var saved = _channelsSavedHub(target);
    if (saved) return saved;
    for (var i = 0; i < channelsDiscoveredHubs.length; i++) {
        if (String(channelsDiscoveredHubs[i].destination_hash || '').toLowerCase() === target) {
            return channelsDiscoveredHubs[i];
        }
    }
    return { destination_hash: target };
}

function _channelsSelectedRoomView() {
    if (channelsHistorySelection) {
        return {
            name: channelsHistorySelection.room_name,
            phase: 'history',
            history_only: true,
            members: [],
            transcript: []
        };
    }
    return channelsActiveRoom ? _channelsRoomByName(channelsActiveRoom) : null;
}

function _channelsHistoryContext(room) {
    if (!room) return null;
    if (room.history_only && channelsHistorySelection) {
        return {
            key: _channelsHistoryKey(
                channelsHistorySelection.hub_destination_hash,
                channelsHistorySelection.room_name
            ),
            hub_destination_hash: channelsHistorySelection.hub_destination_hash,
            room_name: channelsHistorySelection.room_name,
            history_only: true
        };
    }
    var hub = channelsSnapshot.hub && channelsSnapshot.hub.destination_hash;
    if (!hub) return null;
    return {
        key: _channelsHistoryKey(hub, room.name),
        hub_destination_hash: String(hub).toLowerCase(),
        room_name: room.name,
        history_only: false
    };
}

function _channelsAddLocalRoomEvent(roomName, text) {
    var room = String(roomName || '').trim().toLowerCase();
    if (!room || !text) return;
    var events = _channelsLocalRoomEvents[room] || [];
    events.push({
        id: 'local-channel-' + (++_channelsLocalEventSeq),
        kind: 'system',
        timestamp_ms: Date.now(),
        source_hash: null,
        nickname: null,
        text: text,
        ours: true,
        mentioned: false
    });
    if (events.length > 20) events.splice(0, events.length - 20);
    _channelsLocalRoomEvents[room] = events;
}

function _channelsSavedHub(destinationHash) {
    for (var i = 0; i < channelsSavedHubs.length; i++) {
        if (channelsSavedHubs[i].destination_hash === destinationHash) return channelsSavedHubs[i];
    }
    return null;
}

function _channelsMergedHubs() {
    var byHash = {};
    channelsSavedHubs.forEach(function(hub) {
        byHash[hub.destination_hash] = {
            destination_hash: hub.destination_hash,
            announced_name: hub.label || null,
            label: hub.label || null,
            nickname: hub.nickname || '',
            hops: null,
            last_seen: hub.last_connected || hub.added_at || 0,
            saved: true,
            nearby: false
        };
    });
    channelsDiscoveredHubs.forEach(function(hub) {
        var current = byHash[hub.destination_hash] || {};
        byHash[hub.destination_hash] = {
            destination_hash: hub.destination_hash,
            identity_hash: hub.identity_hash || current.identity_hash || null,
            announced_name: hub.announced_name || current.announced_name || null,
            label: current.label || null,
            nickname: current.nickname || '',
            hops: hub.hops,
            last_seen: hub.last_seen || current.last_seen || 0,
            saved: !!current.saved,
            nearby: true
        };
    });
    var ownedDestination = typeof channelHubOwnDestinationHash === 'function'
        ? channelHubOwnDestinationHash()
        : '';
    return Object.keys(byHash).filter(function(key) {
        return !ownedDestination || key.toLowerCase() !== ownedDestination;
    }).map(function(key) { return byHash[key]; }).sort(function(a, b) {
        if (a.nearby !== b.nearby) return a.nearby ? -1 : 1;
        // Closer hubs first: fewer hops is a cheaper, more reliable path. A
        // saved hub we have not heard has no hop count and sorts after ones
        // we have, then by recency.
        var aHops = typeof a.hops === 'number' ? a.hops : Infinity;
        var bHops = typeof b.hops === 'number' ? b.hops : Infinity;
        if (aHops !== bHops) return aHops - bHops;
        return (b.last_seen || 0) - (a.last_seen || 0);
    });
}

function _channelsSelectedHubDestination() {
    return String(
        channelsSnapshot.selected_hub_destination ||
        (channelsSnapshot.hub && channelsSnapshot.hub.destination_hash) ||
        ''
    ).trim().toLowerCase();
}

// One stable navigation model for the measured one-live-hub product budget.
// The selected hub is pinned separately; recently heard hubs and saved-only
// hubs remain visible choices without implying background presence.
function _channelsHubSwitcherModel() {
    var selected = _channelsSelectedHubDestination();
    var merged = _channelsMergedHubs();
    var current = null;
    var nearby = [];
    var saved = [];

    merged.forEach(function(hub) {
        if (selected && hub.destination_hash === selected) {
            current = Object.assign({}, hub);
        } else if (hub.nearby) {
            nearby.push(hub);
        } else if (hub.saved) {
            saved.push(hub);
        }
    });

    if (selected) {
        var observed = channelsSnapshot.hub &&
            String(channelsSnapshot.hub.destination_hash || '').toLowerCase() === selected
            ? channelsSnapshot.hub
            : null;
        current = Object.assign(
            { destination_hash: selected, saved: false, nearby: false },
            current || {},
            observed || {}
        );
        current.destination_hash = selected;
    }

    return {
        current: current,
        nearby: nearby,
        saved: saved
    };
}

function _channelsConnectCommandBlocked() {
    return channelsSnapshot.phase === 'resolving' ||
        channelsSnapshot.phase === 'connecting' ||
        channelsSnapshot.phase === 'awaiting_welcome';
}

function _channelsHubConnectMode(destinationHash) {
    var destination = String(destinationHash || '').trim().toLowerCase();
    var current = _channelsSelectedHubDestination();
    var same = !!destination && !!current && destination === current;

    if (_channelsConnectCommandBlocked()) {
        return {
            kind: 'pending',
            current_destination: current,
            same_destination: same
        };
    }
    if (same && _channelsIsConnected()) {
        return {
            kind: 'current',
            current_destination: current,
            same_destination: true
        };
    }
    if (same && channelsSnapshot.phase === 'reconnecting') {
        return {
            kind: 'recovering',
            current_destination: current,
            same_destination: true
        };
    }
    if (current && destination && destination !== current &&
            (_channelsIsConnected() || channelsSnapshot.phase === 'reconnecting')) {
        return {
            kind: 'switch',
            current_destination: current,
            same_destination: false
        };
    }
    return {
        kind: 'connect',
        current_destination: current,
        same_destination: same
    };
}

function _channelsSetText(id, value) {
    var el = _channelsEl(id);
    if (el) el.textContent = value == null ? '' : String(value);
}

function _channelsSnapshotVersion(snapshot) {
    if (!snapshot || typeof snapshot !== 'object') return null;
    var generation = Number(snapshot.generation);
    var revision = Number(snapshot.revision);
    if (!Number.isSafeInteger(generation) || generation < 0 ||
            !Number.isSafeInteger(revision) || revision < 0) return null;
    return { generation: generation, revision: revision };
}

function _channelsSnapshotIsNewer(snapshot, current) {
    var incoming = _channelsSnapshotVersion(snapshot);
    if (!incoming) return false;
    var existing = _channelsSnapshotVersion(current);
    if (!existing) return true;
    if (incoming.generation !== existing.generation) {
        return incoming.generation > existing.generation;
    }
    return incoming.revision > existing.revision;
}

function channelsApplySnapshot(snapshot) {
    if (!_channelsSnapshotIsNewer(snapshot, channelsSnapshot)) return false;
    var oldHub = channelsSnapshot.hub && channelsSnapshot.hub.destination_hash;
    channelsSnapshot = snapshot;
    if (!Array.isArray(channelsSnapshot.rooms)) channelsSnapshot.rooms = [];
    if (!Array.isArray(channelsSnapshot.hubs)) channelsSnapshot.hubs = [];
    if (!channelsSnapshot.directory) {
        channelsSnapshot.directory = {
            phase: 'idle',
            rooms: [],
            complete: false,
            omitted_count: 0,
            refreshed_at_ms: null,
            last_error: null
        };
    }
    if (!Array.isArray(channelsSnapshot.directory.rooms)) {
        channelsSnapshot.directory.rooms = [];
    }
    if (!channelsSnapshot.hub_greeting) channelsSnapshot.hub_greeting = null;
    channelsSnapshot.hubs.forEach(function(hub) {
        if (hub && hub.observed && !hub.observed.greeting) {
            hub.observed.greeting = null;
        }
    });
    if (!Array.isArray(channelsSnapshot.notices)) channelsSnapshot.notices = [];
    if (!channelsSnapshot.history) {
        channelsSnapshot.history = {
            phase: 'unavailable',
            pending_events: 0,
            dropped_events: 0,
            last_error: null
        };
    }

    var newHub = channelsSnapshot.hub && channelsSnapshot.hub.destination_hash;
    if (channelsPendingShareJoin &&
            Number(channelsSnapshot.generation) !== channelsPendingShareJoin.generation) {
        channelsPendingShareJoin = null;
    }
    var selectedHub = String(
        channelsSnapshot.selected_hub_destination || ''
    ).toLowerCase();
    if (channelsPendingShareJoin && selectedHub &&
            selectedHub !== channelsPendingShareJoin.destination_hash) {
        channelsPendingShareJoin = null;
    }
    if (newHub !== oldHub) {
        _channelsDirectoryRequestSeq += 1;
        _channelsDirectoryRefreshPromise = null;
        _channelsLocalRoomEvents = {};
        _channelsExpandedPresenceGroups = {};
        _channelsSelectedMemberKey = null;
        _channelsMemberReturnFocusKey = null;
    }
    if (newHub && newHub !== oldHub) {
        channelsSavedRooms = [];
        _channelsSavedRoomsHub = null;
        _channelsSavedRoomKeys = {};
        channelsLoadSavedRooms(newHub);
    }
    if (oldHub && !newHub) channelsRefreshRoomIndex();

    if (channelsActiveRoom && !_channelsRoomByName(channelsActiveRoom)) {
        channelsActiveRoom = null;
    }
    if (channelsHistorySelection &&
            String(channelsHistorySelection.hub_destination_hash).toLowerCase() ===
                String(newHub || '').toLowerCase() &&
            _channelsRoomByName(channelsHistorySelection.room_name)) {
        channelsActiveRoom = channelsHistorySelection.room_name;
        channelsHistorySelection = null;
    }
    if (!channelsActiveRoom && !channelsHistorySelection && channelsSnapshot.rooms.length) {
        channelsActiveRoom = channelsSnapshot.rooms[0].name;
    }
    Object.keys(_channelsLocalRoomEvents).forEach(function(roomName) {
        if (!_channelsRoomByName(roomName)) delete _channelsLocalRoomEvents[roomName];
    });

    _channelsPersistConveniences();
    renderChannels();
    if (newHub && _channelsViewVisible() && _channelsDirectoryNeedsRefresh()) {
        channelsRefreshDirectory(false);
    }
    if (channelsPendingShareJoin && channelsSnapshot.phase === 'active') {
        var pendingShare = channelsPendingShareJoin;
        if (String(newHub || '').toLowerCase() === pendingShare.destination_hash) {
            var pendingRoom = _channelsRoomByName(pendingShare.room);
            var pendingRoomTransitioning = pendingRoom &&
                (pendingRoom.phase === 'joining' ||
                    pendingRoom.phase === 'parting');
            if (!pendingRoomTransitioning) {
                channelsPendingShareJoin = null;
                setTimeout(function() {
                    if (pendingRoom && pendingRoom.phase === 'joined') {
                        channelsSelectRoom(pendingShare.room);
                    } else {
                        channelsOpenJoinSheet(pendingShare.room);
                    }
                }, 220);
            }
        }
    } else if (channelsPendingShareJoin && channelsSnapshot.phase === 'error') {
        channelsPendingShareJoin = null;
    }
    var selectedRoom = _channelsSelectedRoomView();
    var selectedHistory = _channelsHistoryContext(selectedRoom);
    var writer = channelsSnapshot.history || {};
    if (selectedHistory && selectedRoom && !selectedRoom.history_only &&
            Number(writer.pending_events) === 0 &&
            (writer.phase === 'ready' || writer.phase === 'degraded')) {
        _channelsScheduleHistorySync(selectedHistory);
    }
    if (typeof channelHubRenderHome === 'function') channelHubRenderHome();
    return true;
}

function channelsLoad(force) {
    var now = Date.now();
    if (!force && _channelsLoadPromise) return _channelsLoadPromise;
    if (!force && _channelsLoadedAt && now - _channelsLoadedAt < 1500) {
        renderChannels();
        return Promise.resolve(channelsSnapshot);
    }
    channelsRefreshUnread();

    var roomIndexRequest = ++_channelsRoomIndexRequestSeq;
    var roomIndexEpoch = _channelsHistoryEpoch;
    _channelsLoadPromise = Promise.all([
        RS.invoke('api_channels'),
        RS.invoke('api_saved_channel_hubs').catch(function() { return []; }),
        typeof channelHubLoad === 'function'
            ? channelHubLoad(force).catch(function() { return null; })
            : Promise.resolve(null),
        RS.invoke('api_channel_room_index').catch(function() { return []; })
    ]).then(function(results) {
        _channelsLoadedAt = Date.now();
        channelsSavedHubs = Array.isArray(results[1]) ? results[1] : [];
        if (roomIndexRequest === _channelsRoomIndexRequestSeq &&
                roomIndexEpoch === _channelsHistoryEpoch) {
            channelsRoomIndex = Array.isArray(results[3]) ? results[3] : [];
        }
        channelsApplySnapshot(results[0]);
        if (channelsSnapshot.hub && channelsSnapshot.hub.destination_hash) {
            channelsLoadSavedRooms(channelsSnapshot.hub.destination_hash);
        }
        if (!_channelsIsConnected() && Date.now() - _channelsLastHubRefreshAt > 5000) {
            channelsRefreshAvailableHubs();
        }
        return channelsSnapshot;
    }).catch(function(error) {
        // A failed refresh must not mutate an already-versioned live snapshot:
        // a later equal API response would correctly be treated as idempotent
        // and could not heal that local-only regression.
        var current = _channelsSnapshotVersion(channelsSnapshot);
        if (!current || current.generation === 0) {
            channelsSnapshot.phase = 'unavailable';
            channelsSnapshot.last_error = error && error.message ? error.message : 'Channels are unavailable';
        }
        renderChannels();
        return channelsSnapshot;
    }).then(function(result) {
        _channelsLoadPromise = null;
        return result;
    });
    return _channelsLoadPromise;
}

function channelsRefreshRoomIndex() {
    var request = ++_channelsRoomIndexRequestSeq;
    var epoch = _channelsHistoryEpoch;
    return RS.invoke('api_channel_room_index').then(function(entries) {
        if (request !== _channelsRoomIndexRequestSeq || epoch !== _channelsHistoryEpoch) {
            return channelsRoomIndex;
        }
        channelsRoomIndex = Array.isArray(entries) ? entries : [];
        renderChannels();
        return channelsRoomIndex;
    }).catch(function() {
        return channelsRoomIndex;
    });
}

function channelsRefreshAvailableHubs() {
    if (_channelsHubRefreshPromise) return _channelsHubRefreshPromise;
    _channelsHubRefreshPromise = RS.invoke('discover_channel_hubs').then(function(hubs) {
        channelsDiscoveredHubs = Array.isArray(hubs) ? hubs : [];
        _channelsLastHubRefreshAt = Date.now();
        renderChannels();
        return channelsDiscoveredHubs;
    }).catch(function() {
        return [];
    }).then(function(result) {
        _channelsHubRefreshPromise = null;
        return result;
    });
    return _channelsHubRefreshPromise;
}

function channelsRefreshDirectory(force) {
    if (!_channelsIsConnected() || !channelsSnapshot.hub) {
        return Promise.resolve(channelsSnapshot);
    }
    if (_channelsDirectoryRefreshPromise) return _channelsDirectoryRefreshPromise;
    if (!force && !_channelsDirectoryNeedsRefresh()) {
        return Promise.resolve(channelsSnapshot);
    }

    var destination = channelsSnapshot.hub.destination_hash;
    var request = ++_channelsDirectoryRequestSeq;
    var pending = RS.invoke('refresh_channel_directory').then(function(snapshot) {
        if (request !== _channelsDirectoryRequestSeq) return channelsSnapshot;
        var currentHub = channelsSnapshot.hub && channelsSnapshot.hub.destination_hash;
        if (currentHub !== destination) return channelsSnapshot;
        channelsApplySnapshot(snapshot);
        return channelsSnapshot;
    }).catch(function() {
        // The manager projects request failures into its authoritative
        // snapshot. Never invent a competing browser-local directory phase.
        return channelsSnapshot;
    }).then(function(result) {
        if (request === _channelsDirectoryRequestSeq) {
            _channelsDirectoryRefreshPromise = null;
        }
        return result;
    });
    _channelsDirectoryRefreshPromise = pending;
    return pending;
}

function channelsLoadSavedRooms(destinationHash) {
    var destination = String(destinationHash || '').toLowerCase();
    if (!destination || _channelsSavedRoomsHub === destination) return Promise.resolve(channelsSavedRooms);
    _channelsSavedRoomsHub = destination;
    return RS.invoke('api_saved_channel_rooms', {
        args: { hub_destination_hash: destination }
    }).then(function(rooms) {
        if (_channelsSavedRoomsHub !== destination) return channelsSavedRooms;
        channelsSavedRooms = Array.isArray(rooms) ? rooms : [];
        var otherHubs = channelsRoomIndex.filter(function(entry) {
            return entry.hub_destination_hash !== destination;
        });
        var localByRoom = {};
        channelsRoomIndex.forEach(function(entry) {
            if (entry.hub_destination_hash !== destination || !entry.has_history) return;
            localByRoom[entry.room_name] = Object.assign({}, entry, { saved: false });
        });
        channelsSavedRooms.forEach(function(saved) {
            var indexed = localByRoom[saved.room_name] || {
                hub_destination_hash: destination,
                room_name: saved.room_name,
                latest_recorded_at_ms: null,
                has_history: false
            };
            indexed.saved = true;
            indexed.last_joined = saved.last_joined;
            localByRoom[saved.room_name] = indexed;
        });
        channelsRoomIndex = otherHubs.concat(Object.keys(localByRoom).map(function(roomName) {
            return localByRoom[roomName];
        }));
        renderChannels();
        return channelsSavedRooms;
    }).catch(function() {
        if (_channelsSavedRoomsHub === destination) channelsSavedRooms = [];
        return [];
    });
}

function _channelsHistoryEntry(context) {
    if (!context || !context.key) return null;
    if (!_channelsHistoryCache[context.key]) {
        _channelsHistoryCache[context.key] = {
            items: [],
            loaded: false,
            loading: false,
            error: null,
            has_more: false,
            next_before: null,
            latest_sequence: '0',
            request_id: 0,
            syncing: false,
            sync_id: 0,
            sync_timer: null,
            marking: false,
            mark_requested: false,
            marked_sequence: '0'
        };
    }
    return _channelsHistoryCache[context.key];
}

function _channelsNormalizeHistoryItem(item) {
    item = item || {};
    return {
        id: String(item.event_id || item.id || ''),
        sequence: String(item.sequence || ''),
        kind: item.kind || 'message',
        timestamp_ms: Number(item.timestamp_ms) || 0,
        recorded_at_ms: Number(item.recorded_at_ms) || 0,
        source_hash: item.source_hash || null,
        nickname: item.nickname || null,
        text: String(item.text || ''),
        ours: !!item.ours,
        mentioned: !!item.mentioned
    };
}

function _channelsMergeHistoryItems(older, newer) {
    var seen = {};
    var merged = [];
    (older || []).concat(newer || []).forEach(function(item) {
        var normalized = _channelsNormalizeHistoryItem(item);
        if (!normalized.id || seen[normalized.id]) return;
        seen[normalized.id] = true;
        merged.push(normalized);
    });
    if (merged.length > CHANNEL_HISTORY_CACHE_ROOM_LIMIT) {
        merged.splice(0, merged.length - CHANNEL_HISTORY_CACHE_ROOM_LIMIT);
    }
    return merged;
}

function _channelsCurrentHistoryKey() {
    var room = _channelsSelectedRoomView();
    var context = _channelsHistoryContext(room);
    return context ? context.key : '';
}

function _channelsLoadHistory(context, older) {
    var entry = _channelsHistoryEntry(context);
    if (!entry || entry.loading) return Promise.resolve(entry);
    if (older && (!entry.loaded || !entry.has_more || !entry.next_before)) {
        return Promise.resolve(entry);
    }
    if (!older && entry.loaded) return Promise.resolve(entry);

    var requestId = ++_channelsHistoryRequestSeq;
    var requestEpoch = _channelsHistoryEpoch;
    var transcript = _channelsEl('channel-transcript');
    var restore = older && transcript && _channelsCurrentHistoryKey() === context.key ? {
        key: context.key,
        scroll_height: transcript.scrollHeight,
        scroll_top: transcript.scrollTop
    } : null;
    entry.loading = true;
    entry.error = null;
    entry.request_id = requestId;

    return RS.invoke('api_channel_history', {
        args: {
            hub_destination_hash: context.hub_destination_hash,
            room: context.room_name,
            before: older ? entry.next_before : null,
            after: null,
            limit: CHANNEL_HISTORY_PAGE_SIZE
        }
    }).then(function(page) {
        if (requestEpoch !== _channelsHistoryEpoch || entry.request_id !== requestId) {
            return entry;
        }
        page = page || {};
        var pageItems = Array.isArray(page.items) ? page.items : [];
        entry.items = older
            ? _channelsMergeHistoryItems(pageItems, entry.items)
            : _channelsMergeHistoryItems([], pageItems);
        entry.loaded = true;
        entry.has_more = !!page.has_more;
        entry.next_before = page.next_before == null ? null : String(page.next_before);
        if (!older) {
            entry.latest_sequence = page.next_after == null
                ? '0' : String(page.next_after);
        }
        entry.error = null;
        return entry;
    }).catch(function(error) {
        if (requestEpoch === _channelsHistoryEpoch && entry.request_id === requestId) {
            entry.error = 'Local history could not be loaded.';
            window.RS.diag('warn', '[Channels] local history query failed:', error);
        }
        return entry;
    }).then(function(result) {
        if (requestEpoch !== _channelsHistoryEpoch || entry.request_id !== requestId) {
            return result;
        }
        entry.loading = false;
        if (_channelsCurrentHistoryKey() === context.key) {
            _channelsRenderRoom(restore);
        }
        var writer = channelsSnapshot.history || {};
        if (!older && Number(writer.pending_events) === 0 &&
                (writer.phase === 'ready' || writer.phase === 'degraded')) {
            _channelsScheduleHistorySync(context);
        } else if (!older && Number(writer.pending_events) === 0 &&
                writer.phase === 'unavailable') {
            _channelsMaybeMarkRoomRead(context, entry);
        }
        return result;
    });
}

function _channelsScheduleHistorySync(context) {
    var entry = _channelsHistoryEntry(context);
    if (!entry || !entry.loaded || entry.loading || entry.syncing || entry.sync_timer) return;
    var epoch = _channelsHistoryEpoch;
    entry.sync_timer = setTimeout(function() {
        entry.sync_timer = null;
        if (epoch === _channelsHistoryEpoch) _channelsSyncHistory(context);
    }, 80);
}

function _channelsSyncHistory(context) {
    var entry = _channelsHistoryEntry(context);
    if (!entry || !entry.loaded || entry.loading || entry.syncing) {
        return Promise.resolve(entry);
    }
    var epoch = _channelsHistoryEpoch;
    var syncId = ++_channelsHistoryRequestSeq;
    var pages = 0;
    var needsAnotherPass = false;
    var changed = false;
    entry.syncing = true;
    entry.sync_id = syncId;

    function nextPage(after) {
        return RS.invoke('api_channel_history', {
            args: {
                hub_destination_hash: context.hub_destination_hash,
                room: context.room_name,
                before: null,
                after: after,
                limit: CHANNEL_HISTORY_SYNC_PAGE_SIZE
            }
        }).then(function(page) {
            if (epoch !== _channelsHistoryEpoch || entry.sync_id !== syncId) return entry;
            page = page || {};
            var pageItems = Array.isArray(page.items) ? page.items : [];
            entry.items = _channelsMergeHistoryItems(entry.items, pageItems);
            var nextAfter = page.next_after == null ? after : String(page.next_after);
            if (nextAfter !== after) {
                entry.latest_sequence = nextAfter;
                changed = true;
            }
            pages++;
            if (page.has_more && nextAfter !== after) {
                if (pages < CHANNEL_HISTORY_MAX_SYNC_PAGES) return nextPage(nextAfter);
                needsAnotherPass = true;
            }
            return entry;
        });
    }

    return nextPage(String(entry.latest_sequence || '0')).catch(function(error) {
        window.RS.diag('warn', '[Channels] local history catch-up failed:', error);
        return entry;
    }).then(function(result) {
        if (epoch !== _channelsHistoryEpoch || entry.sync_id !== syncId) return result;
        entry.syncing = false;
        if (changed && _channelsCurrentHistoryKey() === context.key) _channelsRenderRoom();
        if (needsAnotherPass) {
            _channelsScheduleHistorySync(context);
        } else {
            _channelsMaybeMarkRoomRead(context, entry);
        }
        return result;
    });
}

function _channelsContextIsVisible(context) {
    if (!context || context.key !== _channelsCurrentHistoryKey()) return false;
    if (document.visibilityState && document.visibilityState !== 'visible') return false;
    if (typeof currentView !== 'undefined' && currentView !== 'channels') return false;
    if (_channelsCompact()) {
        var top = RS.viewStack && RS.viewStack.top ? RS.viewStack.top() : null;
        if (!top || top.viewId !== 'channel-detail') return false;
    }
    return true;
}

function _channelsMaybeMarkRoomRead(context, entry) {
    entry = entry || _channelsHistoryEntry(context);
    if (!entry || !entry.loaded || entry.loading || entry.syncing ||
            !_channelsContextIsVisible(context)) return;
    var writer = channelsSnapshot.history || {};
    if (Number(writer.pending_events) > 0 || writer.phase === 'pending') return;
    var through = String(entry.latest_sequence || '0');
    if (!/^[1-9][0-9]*$/.test(through) || entry.marked_sequence === through) return;
    if (entry.marking) {
        entry.mark_requested = true;
        return;
    }
    var epoch = _channelsHistoryEpoch;
    entry.marking = true;
    entry.mark_requested = false;
    RS.invoke('mark_channel_room_read', {
        args: {
            hub_destination_hash: context.hub_destination_hash,
            room: context.room_name,
            through: through
        }
    }).then(function() {
        if (epoch !== _channelsHistoryEpoch) return;
        entry.marked_sequence = through;
        channelsRefreshUnread();
    }).catch(function(error) {
        window.RS.diag('warn', '[Channels] could not advance room read state:', error);
    }).then(function() {
        if (epoch !== _channelsHistoryEpoch) return;
        entry.marking = false;
        if (entry.mark_requested || String(entry.latest_sequence || '0') !== through) {
            entry.mark_requested = false;
            setTimeout(function() { channelsPrepareVisibleRead(); }, 0);
        }
    });
}

function channelsPrepareVisibleRead() {
    var room = _channelsSelectedRoomView();
    var context = _channelsHistoryContext(room);
    if (!context || !_channelsContextIsVisible(context)) return;
    var entry = _channelsEnsureHistory(context);
    if (!entry || !entry.loaded || entry.loading || entry.syncing) return;
    var writer = channelsSnapshot.history || {};
    if (Number(writer.pending_events) > 0 || writer.phase === 'pending') return;
    if (writer.phase === 'ready' || writer.phase === 'degraded') {
        _channelsSyncHistory(context);
    } else {
        _channelsMaybeMarkRoomRead(context, entry);
    }
}
window.channelsPrepareVisibleRead = channelsPrepareVisibleRead;

function _channelsEnsureHistory(context) {
    var entry = _channelsHistoryEntry(context);
    if (entry && !entry.loaded && !entry.loading && !entry.error) {
        _channelsLoadHistory(context, false);
    }
    return entry;
}

// Re-read the saved hubs after the backend retires a superseded identity name
// from them, so an already-loaded sheet stops offering the old nickname.
function channelsRefreshSavedHubs() {
    return RS.invoke('api_saved_channel_hubs').then(function(hubs) {
        channelsSavedHubs = Array.isArray(hubs) ? hubs : [];
        return channelsSavedHubs;
    }).catch(function() { return channelsSavedHubs; });
}

function _channelsPersistConveniences() {
    if (channelsSnapshot.phase !== 'active' || !channelsSnapshot.hub) return;
    var hub = channelsSnapshot.hub;
    var destination = hub.destination_hash;
    var nickname = channelsSnapshot.nickname || _channelsDefaultNickname();
    var label = channelsPendingHubLabel || _channelsHubName(hub);
    var saveKey = destination + '\n' + nickname + '\n' + label;

    if (_channelsSaveHubKey !== saveKey) {
        _channelsSaveHubKey = saveKey;
        _channelsSaveHubPromise = RS.invoke('save_channel_hub', {
            args: {
                destination_hash: destination,
                label: label === 'Channel hub' ? '' : label,
                nickname: nickname,
                connected: true
            }
        }).then(function() {
            return RS.invoke('api_saved_channel_hubs').then(function(hubs) {
                channelsSavedHubs = Array.isArray(hubs) ? hubs : [];
            });
        }).catch(function(error) {
            window.RS.diag('warn', '[Channels] could not remember hub:', error);
        });
    }

    channelsSnapshot.rooms.forEach(function(room) {
        if (room.phase !== 'joined') return;
        var roomKey = destination + '\n' + room.name;
        if (_channelsSavedRoomKeys[roomKey]) return;
        _channelsSavedRoomKeys[roomKey] = true;
        Promise.resolve(_channelsSaveHubPromise).then(function() {
            return RS.invoke('save_channel_room', {
                args: {
                    hub_destination_hash: destination,
                    room: room.name,
                    joined: true
                }
            });
        }).then(function() {
            _channelsSavedRoomsHub = null;
            channelsLoadSavedRooms(destination);
        }).catch(function(error) {
            delete _channelsSavedRoomKeys[roomKey];
            window.RS.diag('warn', '[Channels] could not remember room:', error);
        });
    });
}

function renderChannels() {
    if (!_channelsEl('channels-layout')) return;
    _channelsRenderHubStrip();
    _channelsRenderList();
    _channelsRenderRoom();
    _channelsUpdateMobileMode();
}

function _channelsRenderHubStrip() {
    var strip = _channelsEl('channel-hub-strip');
    var switcher = _channelsEl('channel-hub-switcher-btn');
    var menu = _channelsEl('channel-hub-menu-btn');
    if (!strip) return;

    strip.dataset.phase = channelsSnapshot.phase || 'unavailable';
    if (menu) menu.hidden = channelsSnapshot.phase === 'offline' || channelsSnapshot.phase === 'unavailable';

    var hub = channelsSnapshot.hub;
    var title = _channelsPhaseLabel(channelsSnapshot.phase);
    var meta = channelsSnapshot.last_error || 'Choose a hub to begin';
    if (hub) {
        title = _channelsHubName(hub);
        if (channelsSnapshot.phase === 'active') {
            meta = 'Connected as ' + (channelsSnapshot.nickname || 'guest');
        } else if (channelsSnapshot.phase === 'stale') {
            meta = 'Link is recovering';
        } else if (_channelsIsConnecting()) {
            meta = _channelsPhaseLabel(channelsSnapshot.phase) + '\u2026';
        } else if (channelsSnapshot.last_error) {
            meta = channelsSnapshot.last_error;
        } else {
            meta = _channelsShortHash(hub.destination_hash);
        }
    }
    _channelsSetText('channel-hub-strip-title', title);
    _channelsSetText('channel-hub-strip-meta', meta);
    if (switcher) {
        var currentName = hub ? _channelsHubName(hub) : '';
        switcher.setAttribute(
            'aria-label',
            currentName
                ? 'Choose a channel hub. Current selection: ' + currentName
                : 'Choose a channel hub'
        );
        switcher.title = currentName ? 'Switch channel hub' : 'Choose a channel hub';
    }
}

function _channelsRenderList() {
    var list = _channelsEl('channels-list');
    var label = _channelsEl('channels-list-label');
    var join = _channelsEl('channels-join-btn');
    if (!list) return;
    list.textContent = '';

    if (_channelsIsConnected()) {
        if (label) label.textContent = 'Channels';
        if (join) join.hidden = false;
        var liveNames = {};
        channelsSnapshot.rooms.forEach(function(room) {
            liveNames[room.name] = true;
        });
        var savedOnlyRooms = channelsSavedRooms.filter(function(saved) {
            return !liveNames[saved.room_name];
        });
        if (channelsSnapshot.rooms.length || savedOnlyRooms.length) {
            list.appendChild(_channelsListSection('Your channels'));
        }
        channelsSnapshot.rooms.forEach(function(room) {
            list.appendChild(_channelsBuildRoomRow(room, false));
        });
        savedOnlyRooms.forEach(function(saved) {
            var indexed = channelsRoomIndex.find(function(entry) {
                return entry.hub_destination_hash === saved.hub_destination_hash &&
                    entry.room_name === saved.room_name;
            });
            list.appendChild(_channelsBuildRoomRow({ name: saved.room_name }, true, {
                hub_destination_hash: saved.hub_destination_hash,
                has_history: !!(indexed && indexed.has_history)
            }));
        });

        var directory = channelsSnapshot.directory || {};
        var directoryRooms = Array.isArray(directory.rooms) ? directory.rooms : [];
        var knownNames = Object.assign({}, liveNames);
        savedOnlyRooms.forEach(function(saved) { knownNames[saved.room_name] = true; });
        var availableRooms = directoryRooms.filter(function(room) {
            return room && room.name && !knownNames[room.name];
        });
        list.appendChild(_channelsListSection('Public on this hub', {
            className: 'channel-directory-section',
            actionText: directory.phase === 'loading' ? 'Checking\u2026' : 'Refresh',
            actionDisabled: directory.phase === 'loading',
            action: function() { channelsRefreshDirectory(true); }
        }));
        availableRooms.forEach(function(room) {
            list.appendChild(_channelsBuildDirectoryRoomRow(room));
        });
        if (directory.phase === 'loading') {
            list.appendChild(_channelsDirectoryStatus(
                availableRooms.length ? 'Refreshing public channels\u2026' : 'Checking this hub\u2026'
            ));
        } else if (directory.phase === 'error') {
            list.appendChild(_channelsDirectoryStatus(
                'Could not refresh public channels',
                directory.last_error || 'Try again when the Link is available',
                'error'
            ));
        } else if (directory.phase === 'idle') {
            list.appendChild(_channelsDirectoryStatus(
                'Public channels have not been requested yet',
                'Refresh to ask this hub'
            ));
        } else if (!availableRooms.length) {
            list.appendChild(_channelsDirectoryStatus(
                directoryRooms.length
                    ? 'All advertised channels are already in your list'
                    : 'No public channels advertised'
            ));
        }
        if (directory.complete === false && Number(directory.omitted_count) > 0) {
            var omitted = Number(directory.omitted_count);
            list.appendChild(_channelsDirectoryStatus(
                omitted + (omitted === 1
                    ? ' more channel was not included'
                    : ' more channels were not included'),
                'The hub kept its response within one constrained packet',
                'warning'
            ));
        }
        return;
    }

    var recentRooms = channelsRoomIndex.slice().sort(function(a, b) {
        var bRecent = Number(b.latest_recorded_at_ms) || (Number(b.last_joined) || 0) * 1000;
        var aRecent = Number(a.latest_recorded_at_ms) || (Number(a.last_joined) || 0) * 1000;
        return bRecent - aRecent;
    });
    if (label) label.textContent = recentRooms.length ? 'Browse' : 'Available hubs';
    if (join) join.hidden = true;
    var hubs = _channelsMergedHubs();
    if (hubs.length && recentRooms.length) {
        list.appendChild(_channelsListSection('Available hubs'));
    }
    hubs.forEach(function(hub) { list.appendChild(_channelsBuildHubRow(hub)); });
    if (!hubs.length && !recentRooms.length) {
        var ownHubReady = _channelsOwnedHubReady();
        var emptyText = _channelsIsConnecting()
            ? 'Connecting to hub\u2026'
            : (ownHubReady ? 'No other hubs available' : 'No channel hubs yet');
        list.appendChild(_channelsEmptyList(
            emptyText,
            ownHubReady ? null : 'Connect to a hub',
            ownHubReady ? null : 'connect'
        ));
    }
    if (recentRooms.length) {
        list.appendChild(_channelsListSection('On this device'));
        recentRooms.forEach(function(saved) {
            list.appendChild(_channelsBuildRoomRow({ name: saved.room_name }, true, {
                hub_destination_hash: saved.hub_destination_hash,
                has_history: !!saved.has_history,
                hub_label: _channelsTimelineHubName(_channelsHubByDestination(
                    saved.hub_destination_hash
                ))
            }));
        });
    }
}

function _channelsListSection(text, options) {
    options = options || {};
    var section = document.createElement('div');
    section.className = 'channels-list-section' +
        (options.className ? ' ' + options.className : '');
    var label = document.createElement('span');
    label.textContent = text;
    section.appendChild(label);
    if (options.actionText) {
        var action = document.createElement('button');
        action.type = 'button';
        action.className = 'channels-list-section-action';
        action.textContent = options.actionText;
        action.disabled = !!options.actionDisabled;
        action.addEventListener('click', function(event) {
            event.preventDefault();
            event.stopPropagation();
            if (typeof options.action === 'function') options.action();
        });
        section.appendChild(action);
    }
    return section;
}

function _channelsDirectoryStatus(text, detail, tone) {
    var status = document.createElement('div');
    status.className = 'channel-directory-status';
    if (tone) status.dataset.tone = tone;
    var label = document.createElement('span');
    label.textContent = text;
    status.appendChild(label);
    if (detail) {
        var copy = document.createElement('small');
        copy.textContent = detail;
        status.appendChild(copy);
    }
    return status;
}

function _channelsEmptyList(text, actionText, action) {
    var empty = document.createElement('div');
    empty.className = 'channels-list-empty';
    var label = document.createElement('span');
    label.textContent = text;
    empty.appendChild(label);
    if (actionText) {
        var button = document.createElement('button');
        button.type = 'button';
        button.className = 'nr-btn nr-btn-sm';
        button.dataset.channelAction = action;
        button.textContent = actionText;
        empty.appendChild(button);
    }
    return empty;
}

function _channelsRadioIcon() {
    return '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M8.5 16.5a6 6 0 0 1 0-9"/><path d="M15.5 7.5a6 6 0 0 1 0 9"/><circle cx="12" cy="12" r="1.6" fill="currentColor" stroke="none"/></svg>';
}

function _channelsRoomIcon() {
    return '<svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><line x1="9" y1="3" x2="7" y2="21"/><line x1="17" y1="3" x2="15" y2="21"/><line x1="4" y1="9" x2="20" y2="9"/><line x1="3" y1="15" x2="19" y2="15"/></svg>';
}

function _channelsHubMonogram(hub) {
    var name = _channelsHubName(hub).trim();
    var characters = Array.from(name || 'H');
    return (characters[0] || 'H').toLocaleUpperCase();
}

function _channelsHubDistance(hub) {
    if (!hub || !hub.nearby || typeof hub.hops !== 'number') return '';
    if (hub.hops === 0) return 'Direct';
    return hub.hops + (hub.hops === 1 ? ' hop' : ' hops');
}

function _channelsHubMeta(hub) {
    var hash = _channelsShortHash(hub && hub.destination_hash);
    return hub && hub.saved ? 'Saved \u00b7 ' + hash : hash;
}

function _channelsBuildHubMark(hub) {
    var mark = document.createElement('span');
    mark.className = 'channel-hub-row-mark';
    mark.dataset.tone = _channelsIdentityTone(hub && hub.destination_hash);
    mark.textContent = _channelsHubMonogram(hub);
    mark.setAttribute('aria-hidden', 'true');
    return mark;
}

function _channelsBuildHubRow(hub, options) {
    options = options || {};
    var row = document.createElement('button');
    row.type = 'button';
    row.className = 'channel-hub-row' + (options.current ? ' current' : '');
    row.dataset.destinationHash = hub.destination_hash;
    if (options.current) row.setAttribute('aria-current', 'true');
    row.disabled = !!options.disabled;

    row.appendChild(_channelsBuildHubMark(hub));

    var copy = document.createElement('span');
    copy.className = 'channel-hub-row-copy';
    var title = document.createElement('span');
    title.className = 'channel-hub-row-title';
    title.textContent = _channelsHubName(hub);
    var meta = document.createElement('span');
    meta.className = 'channel-hub-row-meta';
    meta.textContent = _channelsHubMeta(hub);
    copy.appendChild(title);
    copy.appendChild(meta);
    row.appendChild(copy);

    var trailing = document.createElement('span');
    trailing.className = 'channel-hub-row-trailing';
    var distance = document.createElement('span');
    distance.className = 'channel-hub-row-distance';
    distance.textContent = _channelsHubDistance(hub);
    if (distance.textContent) trailing.appendChild(distance);
    if (options.status) {
        var status = document.createElement('span');
        status.className = 'channel-row-status' +
            (options.statusTone ? ' ' + options.statusTone : '');
        status.textContent = options.status;
        trailing.appendChild(status);
    }
    if (trailing.childNodes.length) row.appendChild(trailing);
    row.addEventListener('click', function() {
        if (typeof options.onSelect === 'function') options.onSelect(hub);
        else channelsOpenConnectSheet(hub);
    });
    return row;
}

function _channelsBuildDirectoryRoomRow(room) {
    var row = document.createElement('button');
    row.type = 'button';
    row.className = 'channel-room-row channel-directory-room';
    row.dataset.room = room.name;
    row.setAttribute('aria-label', 'Join ' + room.name);

    var icon = document.createElement('span');
    icon.className = 'channel-room-row-icon';
    icon.innerHTML = _channelsRoomIcon();
    row.appendChild(icon);

    var copy = document.createElement('span');
    copy.className = 'channel-room-row-copy';
    var title = document.createElement('span');
    title.className = 'channel-room-row-title';
    title.textContent = room.name;
    var meta = document.createElement('span');
    meta.className = 'channel-room-row-meta';
    meta.textContent = room.topic || 'Public channel on this hub';
    copy.appendChild(title);
    copy.appendChild(meta);
    row.appendChild(copy);

    var action = document.createElement('span');
    action.className = 'channel-row-status joined';
    action.textContent = 'Join';
    row.appendChild(action);
    row.addEventListener('click', function() {
        channelsOpenJoinSheet(room.name);
    });
    return row;
}

function _channelsBuildRoomRow(room, savedOnly, options) {
    options = options || {};
    var roomHub = options.hub_destination_hash ||
        (channelsSnapshot.hub && channelsSnapshot.hub.destination_hash) || '';
    var historyKey = _channelsHistoryKey(roomHub, room.name);
    var selectedHistoryKey = channelsHistorySelection
        ? _channelsHistoryKey(
            channelsHistorySelection.hub_destination_hash,
            channelsHistorySelection.room_name
        )
        : '';
    var row = document.createElement('button');
    row.type = 'button';
    row.className = 'channel-room-row' + (
        (!savedOnly && room.name === channelsActiveRoom) ||
        (savedOnly && historyKey && historyKey === selectedHistoryKey)
            ? ' active' : ''
    );
    row.dataset.room = room.name;
    if (!savedOnly) row.dataset.phase = room.phase || 'joining';

    var icon = document.createElement('span');
    icon.className = 'channel-room-row-icon';
    icon.innerHTML = _channelsRoomIcon();
    row.appendChild(icon);

    var copy = document.createElement('span');
    copy.className = 'channel-room-row-copy';
    var title = document.createElement('span');
    title.className = 'channel-room-row-title';
    title.textContent = room.name;
    var meta = document.createElement('span');
    meta.className = 'channel-room-row-meta';
    if (savedOnly) {
        var localState = options.has_history ? 'stored locally' : 'saved locally';
        meta.textContent = options.hub_label
            ? options.hub_label + ' \u00b7 ' + localState
            : localState.charAt(0).toUpperCase() + localState.slice(1) + ' \u00b7 open';
    } else if (room.last_error) {
        meta.textContent = room.last_error;
    } else if (room.phase === 'joined') {
        var count = Array.isArray(room.members) ? room.members.length : 0;
        meta.textContent = count ? count + (count === 1 ? ' person here' : ' people here') : 'Joined';
    } else {
        meta.textContent = _channelsRoomPhaseLabel(room.phase);
    }
    copy.appendChild(title);
    copy.appendChild(meta);
    row.appendChild(copy);

    var unread = _channelsRoomUnreadState(roomHub, room.name);
    if (unread.unread_count > 0) {
        var unreadBadge = document.createElement('span');
        unreadBadge.className = 'channel-unread-badge' +
            (unread.mention_count > 0 ? ' mention' : '') +
            (unread.notification_level === 'mute' ? ' muted' : '');
        unreadBadge.textContent = unread.unread_count > 99 ? '99+' : String(unread.unread_count);
        unreadBadge.setAttribute(
            'aria-label',
            unread.unread_count + ' unread' +
                (unread.mention_count ? ', ' + unread.mention_count + ' mentions' : '')
        );
        row.classList.add('has-unread');
        if (unread.mention_count > 0) row.classList.add('has-mention');
        row.appendChild(unreadBadge);
    }

    if (savedOnly || room.phase !== 'joined') {
        var status = document.createElement('span');
        status.className = 'channel-row-status' + (!savedOnly && room.phase === 'error' ? ' error' : '');
        status.textContent = savedOnly
            ? (options.has_history ? 'History' : 'Saved')
            : _channelsRoomPhaseLabel(room.phase);
        row.appendChild(status);
    }

    row.addEventListener('click', function() {
        if (savedOnly) {
            channelsSelectHistoryRoom(options.hub_destination_hash, room.name);
        }
        else channelsSelectRoom(room.name);
    });
    return row;
}

function _channelsRenderRoom(scrollRestore) {
    var layout = _channelsEl('channels-layout');
    var header = _channelsEl('channel-room-header');
    var compose = _channelsEl('channel-compose');
    var transcript = _channelsEl('channel-transcript');
    var room = _channelsSelectedRoomView();
    if (!header || !transcript || !compose) return;
    if (layout) layout.classList.toggle('has-active-room', !!room);
    if (layout) layout.classList.toggle('room-live', !!room && room.phase === 'joined');

    if (!room) {
        if (layout) layout.classList.remove('members-open');
        header.hidden = true;
        compose.hidden = true;
        _channelsRenderRoomEmpty(transcript);
        _channelsRenderMembers(null);
        _channelsRenderedRoomKey = '';
        return;
    }

    var historyContext = _channelsHistoryContext(room);
    var historyEntry = _channelsEnsureHistory(historyContext);
    var renderKey = historyContext
        ? historyContext.key + (room.history_only ? '|history' : '|live')
        : 'room|' + room.name;
    var roomChanged = _channelsRenderedRoomKey !== renderKey;
    _channelsRenderedRoomKey = renderKey;

    header.hidden = false;
    compose.hidden = room.phase !== 'joined';
    if (room.phase !== 'joined' && layout) layout.classList.remove('members-open');
    var membersToggle = _channelsEl('channel-members-toggle');
    if (membersToggle) membersToggle.hidden = room.phase !== 'joined';
    var roomMenu = _channelsEl('channel-room-menu-btn');
    if (roomMenu) roomMenu.hidden = false;
    _channelsSetText('channel-room-title', room.name);
    var phase = _channelsEl('channel-room-phase');
    if (phase) {
        phase.dataset.phase = room.phase || 'joining';
        phase.textContent = _channelsRoomPhaseLabel(room.phase);
    }
    var memberCount = Array.isArray(room.members) ? room.members.length : 0;
    var roomMeta;
    if (room.phase === 'joining') {
        roomMeta = 'Join request sent \u00b7 awaiting hub';
    } else if (room.phase === 'parting') {
        roomMeta = 'Leaving channel';
    } else if (room.phase === 'error') {
        roomMeta = room.last_error || 'Join was not confirmed';
    } else if (room.history_only && historyContext) {
        roomMeta = _channelsTimelineHubName(_channelsHubByDestination(
            historyContext.hub_destination_hash
        )) + ' \u00b7 stored on this device';
    } else {
        roomMeta = memberCount ? memberCount + (memberCount === 1 ? ' person here' : ' people here') : 'No member list';
        if (room.topic) roomMeta = room.topic + (memberCount ? ' \u00b7 ' + roomMeta : '');
    }
    _channelsSetText('channel-room-meta', roomMeta);

    var wasNearBottom = transcript.scrollHeight - transcript.scrollTop - transcript.clientHeight < 90;
    transcript.textContent = '';
    if (!room.history_only) {
        channelsSnapshot.notices.forEach(function(item) {
            transcript.appendChild(_channelsBuildHubNotice(item));
        });
    }
    if (historyContext && historyEntry) {
        transcript.appendChild(_channelsBuildHistoryRail(historyContext, historyEntry));
    }
    var items = _channelsTimelineEntries(room, historyEntry);
    var renderedItems = _channelsGroupPresenceEvents(items, room.name);
    if (room.history_only && renderedItems.length) {
        transcript.appendChild(_channelsBuildHistoryOnlyBanner(historyContext));
    } else if (room.phase !== 'joined' && !room.history_only) {
        transcript.appendChild(_channelsBuildRoomTransition(room));
    }
    if (!renderedItems.length && room.history_only) {
        transcript.appendChild(_channelsBuildHistoryEmpty(historyContext, historyEntry));
    } else if (!renderedItems.length && room.phase === 'joined') {
        var waiting = document.createElement('div');
        waiting.className = 'channel-welcome-state';
        var waitingTitle = document.createElement('h3');
        waitingTitle.textContent = 'Ready when you are';
        var waitingCopy = document.createElement('p');
        waitingCopy.textContent = historyEntry && historyEntry.loading
            ? 'Loading this device\u2019s recent activity\u2026'
            : 'Messages will appear here as people post.';
        waiting.appendChild(waitingTitle);
        waiting.appendChild(waitingCopy);
        transcript.appendChild(waiting);
    } else if (renderedItems.length) {
        var previousDay = null;
        renderedItems.forEach(function(entry) {
            var dayItem = entry.presenceGroup && entry.presenceGroup.entries.length
                ? entry.presenceGroup.entries[0].item
                : entry.item;
            var day = _channelsDayKey(dayItem);
            if (day && day !== previousDay) {
                transcript.appendChild(_channelsBuildDaySeparator(dayItem));
                previousDay = day;
            }
            if (entry.presenceGroup) {
                transcript.appendChild(_channelsBuildPresenceGroup(entry.presenceGroup));
            } else {
                transcript.appendChild(_channelsBuildTranscriptItem(entry.item, entry.hubNotice));
            }
        });
    }
    if (scrollRestore && scrollRestore.key === (historyContext && historyContext.key)) {
        requestAnimationFrame(function() {
            transcript.scrollTop = scrollRestore.scroll_top +
                Math.max(0, transcript.scrollHeight - scrollRestore.scroll_height);
        });
    } else if (wasNearBottom || roomChanged) {
        requestAnimationFrame(function() { transcript.scrollTop = transcript.scrollHeight; });
    }
    _channelsRenderMembers(room.history_only ? null : room);
    _channelsUpdateComposer();
}

function _channelsTimelineEntries(room, historyEntry) {
    var entries = [];
    var seen = {};
    var order = 0;
    var historyItems = historyEntry && Array.isArray(historyEntry.items)
        ? historyEntry.items : [];
    var historyById = {};
    historyItems.forEach(function(item) {
        if (item.id) historyById[item.id] = item;
    });

    function append(item, hubNotice) {
        if (!item) return;
        var id = String(item.id || '');
        if (id && seen[id]) return;
        if (id) seen[id] = true;
        entries.push({ item: item, hubNotice: !!hubNotice, order: order++ });
    }

    if (room.history_only) {
        historyItems.forEach(function(item) { append(item, false); });
        return entries;
    }

    var liveItems = Array.isArray(room.transcript) ? room.transcript : [];
    var liveIds = {};
    liveItems.forEach(function(item) {
        if (item && item.id) liveIds[item.id] = true;
    });
    // The native append log is canonical for durable ordering; transcript
    // items that are no longer inside the bounded live window precede that
    // window. Matching live items retain their freshest presentation fields
    // while inheriting the local receive clock used for day boundaries.
    historyItems.forEach(function(item) {
        if (!liveIds[item.id]) append(item, false);
    });
    liveItems.forEach(function(item) {
        var stored = item && item.id ? historyById[item.id] : null;
        var merged = stored ? Object.assign({}, item, {
            sequence: stored.sequence,
            recorded_at_ms: stored.recorded_at_ms,
            mentioned: !!(item.mentioned || stored.mentioned)
        }) : item;
        append(merged, _channelsIsHubNotice(merged));
    });
    (_channelsLocalRoomEvents[room.name] || []).forEach(function(item) {
        append(item, false);
    });
    return entries;
}

function _channelsBuildHistoryRail(context, entry) {
    var rail = document.createElement('div');
    rail.className = 'channel-history-rail';
    rail.dataset.phase = entry.error ? 'error' :
        (channelsSnapshot.history && channelsSnapshot.history.phase) || 'ready';
    rail.setAttribute('role', entry.error ? 'alert' : 'status');

    var label = document.createElement('span');
    label.className = 'channel-history-label';
    label.innerHTML = '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="12" cy="12" r="8"/><path d="M12 8v4l2.5 1.5"/></svg>';
    var text = document.createElement('span');
    var writer = channelsSnapshot.history || {};
    if (entry.error) {
        text.textContent = entry.error;
    } else if (!entry.loaded && entry.loading) {
        text.textContent = 'Loading local timeline\u2026';
    } else if (writer.phase === 'degraded') {
        text.textContent = writer.last_error || 'Some recent activity could not be saved.';
    } else if (writer.phase === 'pending' && Number(writer.pending_events) > 0) {
        text.textContent = 'Saving recent activity on this device\u2026';
    } else {
        text.textContent = 'Local timeline';
    }
    label.appendChild(text);
    rail.appendChild(label);

    if (entry.error || entry.has_more) {
        var action = document.createElement('button');
        action.type = 'button';
        action.className = 'channel-history-action';
        action.disabled = entry.loading;
        action.textContent = entry.loading
            ? 'Loading\u2026'
            : (entry.error ? 'Try again' : 'Load earlier');
        action.addEventListener('click', function() {
            action.disabled = true;
            action.textContent = 'Loading\u2026';
            _channelsLoadHistory(context, !entry.error);
        });
        rail.appendChild(action);
    }
    return rail;
}

function _channelsHistoryActionButton(context) {
    var activeHub = channelsSnapshot.hub && channelsSnapshot.hub.destination_hash;
    var sameHub = _channelsIsConnected() &&
        String(activeHub || '').toLowerCase() ===
            String(context.hub_destination_hash || '').toLowerCase();
    var button = document.createElement('button');
    button.type = 'button';
    button.className = 'nr-btn nr-btn-secondary nr-btn-sm';
    button.textContent = sameHub ? 'Rejoin channel' : 'Connect to hub';
    button.addEventListener('click', function() {
        if (sameHub) {
            channelsOpenJoinSheet(context.room_name);
        } else {
            channelsOpenConnectSheet(_channelsHubByDestination(
                context.hub_destination_hash
            ));
        }
    });
    return button;
}

function _channelsBuildHistoryOnlyBanner(context) {
    var banner = document.createElement('aside');
    banner.className = 'channel-history-banner';
    var copy = document.createElement('span');
    copy.textContent = 'Read-only activity stored on this device.';
    banner.appendChild(copy);
    banner.appendChild(_channelsHistoryActionButton(context));
    return banner;
}

function _channelsBuildHistoryEmpty(context, entry) {
    var state = document.createElement('div');
    state.className = 'channel-welcome-state channel-history-empty';
    var title = document.createElement('h3');
    var copy = document.createElement('p');
    if (entry && entry.loading) {
        title.textContent = 'Loading local history\u2026';
        copy.textContent = 'Ratspeak is reading this room\u2019s bounded timeline from this device.';
    } else if (entry && entry.error) {
        title.textContent = 'History is temporarily unavailable';
        copy.textContent = 'The live channel is separate; retry the local timeline when you are ready.';
    } else {
        title.textContent = 'No local activity yet';
        copy.textContent = 'Activity appears here after this identity receives it in the channel.';
    }
    state.appendChild(title);
    state.appendChild(copy);
    state.appendChild(_channelsHistoryActionButton(context));
    return state;
}

function _channelsActivityTime(item) {
    return Number(item && item.recorded_at_ms) ||
        Number(item && item.timestamp_ms) || 0;
}

function _channelsDayKey(item) {
    var timestamp = _channelsActivityTime(item);
    if (!timestamp) return '';
    var date = new Date(timestamp);
    if (Number.isNaN(date.getTime())) return '';
    return date.getFullYear() + '-' + (date.getMonth() + 1) + '-' + date.getDate();
}

function _channelsBuildDaySeparator(item) {
    var timestamp = _channelsActivityTime(item);
    var date = new Date(timestamp);
    var today = new Date();
    var yesterday = new Date(
        today.getFullYear(),
        today.getMonth(),
        today.getDate() - 1
    );
    var label;
    if (_channelsDayKey({ recorded_at_ms: today.getTime() }) === _channelsDayKey(item)) {
        label = 'Today';
    } else if (_channelsDayKey({ recorded_at_ms: yesterday.getTime() }) === _channelsDayKey(item)) {
        label = 'Yesterday';
    } else {
        label = date.toLocaleDateString([], {
            month: 'short',
            day: 'numeric',
            year: date.getFullYear() === today.getFullYear() ? undefined : 'numeric'
        });
    }
    var separator = document.createElement('div');
    separator.className = 'channel-day-separator';
    separator.setAttribute('role', 'separator');
    var text = document.createElement('span');
    text.textContent = label;
    separator.appendChild(text);
    return separator;
}

function _channelsBuildRoomTransition(room) {
    var card = document.createElement('section');
    card.className = 'channel-transition-card';
    card.dataset.phase = room.phase || 'joining';
    card.setAttribute('role', 'status');
    card.setAttribute('aria-live', 'polite');

    if (room.phase !== 'parting') {
        var rail = document.createElement('div');
        rail.className = 'channel-transition-rail';
        ['Request sent', 'Hub reply', 'Ready'].forEach(function(labelText, index) {
            var step = document.createElement('span');
            step.className = 'channel-transition-step';
            if (index === 0) step.classList.add('complete');
            if (index === 1 && room.phase === 'joining') step.classList.add('current');
            if (index === 1 && room.phase === 'error') step.classList.add('failed');
            var dot = document.createElement('i');
            dot.setAttribute('aria-hidden', 'true');
            var label = document.createElement('span');
            label.textContent = labelText;
            step.appendChild(dot);
            step.appendChild(label);
            rail.appendChild(step);
        });
        card.appendChild(rail);
    }

    var title = document.createElement('h3');
    var copy = document.createElement('p');
    if (room.phase === 'joining') {
        title.textContent = 'Waiting for ' + _channelsHubName(channelsSnapshot.hub);
        copy.textContent = 'Ratspeak sent the request to join ' + room.name + '. Messages unlock when the hub confirms your membership.';
    } else if (room.phase === 'parting') {
        title.textContent = 'Leaving ' + room.name + '\u2026';
        copy.textContent = 'Ratspeak sent the leave request and is waiting for the hub.';
    } else {
        title.textContent = room.name + ' was not confirmed';
        copy.textContent = room.last_error || 'No confirmation arrived from the hub. You can try again without reconnecting.';
    }
    card.appendChild(title);
    card.appendChild(copy);

    if (room.phase === 'joining' || room.phase === 'error') {
        var actions = document.createElement('div');
        actions.className = 'channel-transition-actions';
        if (room.phase === 'error') {
            var retry = document.createElement('button');
            retry.type = 'button';
            retry.className = 'nr-btn nr-btn-primary nr-btn-sm';
            retry.dataset.channelAction = 'retry-room';
            retry.dataset.room = room.name;
            retry.textContent = 'Try again';
            actions.appendChild(retry);
        }
        var cancel = document.createElement('button');
        cancel.type = 'button';
        cancel.className = 'nr-btn nr-btn-secondary nr-btn-sm';
        cancel.dataset.channelAction = 'leave-room';
        cancel.dataset.room = room.name;
        cancel.textContent = room.phase === 'joining' ? 'Cancel join' : 'Leave channel';
        actions.appendChild(cancel);
        card.appendChild(actions);
    }
    return card;
}

function _channelsRenderRoomEmpty(transcript) {
    transcript.textContent = '';
    var state = document.createElement('div');
    state.className = 'channel-welcome-state';
    var mark = document.createElement('div');
    mark.className = 'channel-welcome-mark';
    mark.innerHTML = _channelsRadioIcon().replace('width="18" height="18"', 'width="48" height="48"');
    var title = document.createElement('h3');
    var copy = document.createElement('p');
    var button = document.createElement('button');
    button.type = 'button';
    button.className = 'nr-btn nr-btn-primary';

    if (_channelsIsConnecting()) {
        title.textContent = _channelsPhaseLabel(channelsSnapshot.phase) + '\u2026';
        copy.textContent = 'Ratspeak is establishing an authenticated Reticulum Link and waiting for the hub to welcome this session.';
        button.className = 'nr-btn nr-btn-secondary';
        button.dataset.channelAction = 'disconnect';
        button.textContent = 'Cancel';
    } else if (_channelsIsConnected()) {
        title.textContent = 'Connected. Join a channel.';
        copy.textContent = 'Choose a channel on ' + _channelsHubName(channelsSnapshot.hub) + '.';
        button.dataset.channelAction = 'join';
        button.textContent = 'Join a channel';
        if (channelsSnapshot.hub_greeting) {
            transcript.appendChild(_channelsBuildHubGreeting(channelsSnapshot.hub_greeting));
            state.classList.add('has-hub-greeting');
        }
    } else if (channelsSnapshot.phase === 'error') {
        title.textContent = 'The channel session ended';
        copy.textContent = channelsSnapshot.last_error || 'The channel Link closed. Reconnect when you are ready.';
        button.dataset.channelAction = 'connect';
        button.textContent = 'Reconnect';
    } else {
        if (_channelsOwnedHubReady()) {
            title.textContent = 'Open your hub';
            copy.textContent = 'Enter as the owner, then create or join a channel.';
            button.dataset.channelAction = 'open-owned-hub';
            button.textContent = 'Open hub';
        } else {
            title.textContent = 'Join a conversation';
            copy.textContent = 'Connect to a trusted hub, then choose a channel.';
            button.dataset.channelAction = 'connect';
            button.textContent = 'Find a hub';
        }
    }
    state.appendChild(mark);
    state.appendChild(title);
    state.appendChild(copy);
    state.appendChild(button);
    transcript.appendChild(state);
}

function _channelsBuildHubGreeting(item, compact) {
    var greeting = document.createElement('aside');
    greeting.className = 'channel-hub-greeting' + (compact ? ' compact' : '');
    greeting.setAttribute('aria-label', 'Hub greeting');
    var heading = document.createElement('div');
    heading.className = 'channel-hub-greeting-heading';
    var label = document.createElement('span');
    label.className = 'channel-hub-notice-label';
    label.textContent = 'Hub greeting';
    var hub = document.createElement('span');
    hub.className = 'channel-hub-greeting-source';
    hub.textContent = _channelsHubName(channelsSnapshot.hub);
    var body = document.createElement('div');
    body.className = 'channel-hub-notice-text';
    body.textContent = item.text || '';
    heading.appendChild(label);
    heading.appendChild(hub);
    greeting.appendChild(heading);
    greeting.appendChild(body);
    return greeting;
}

function _channelsIsHubNotice(item) {
    if (!item || item.kind !== 'notice' || item.ours) return false;
    var hubIdentity = channelsSnapshot.hub && channelsSnapshot.hub.identity_hash;
    return !!hubIdentity && String(item.source_hash || '').toLowerCase() === String(hubIdentity).toLowerCase();
}

function _channelsBuildHubNotice(item) {
    var notice = document.createElement('aside');
    notice.className = 'channel-hub-notice';
    var heading = document.createElement('div');
    heading.className = 'channel-hub-notice-heading';
    var label = document.createElement('span');
    label.className = 'channel-hub-notice-label';
    label.textContent = 'Hub';
    var time = document.createElement('time');
    time.className = 'channel-event-time';
    time.dateTime = new Date(Number(item.timestamp_ms) || Date.now()).toISOString();
    time.textContent = _channelsFormatTime(item.timestamp_ms);
    var meta = document.createElement('span');
    meta.className = 'channel-event-meta';
    meta.appendChild(time);
    var quote = _channelsBuildQuoteButton(item, 'Hub');
    if (quote) meta.appendChild(quote);
    var body = document.createElement('div');
    body.className = 'channel-hub-notice-text';
    body.textContent = item.text || '';
    heading.appendChild(label);
    heading.appendChild(meta);
    notice.appendChild(heading);
    notice.appendChild(body);
    return notice;
}

function _channelsBuildQuoteButton(item, authorText) {
    if (!_channelsCanCompose() || !item ||
            ['message', 'action', 'notice'].indexOf(item.kind || 'message') === -1 ||
            !String(item.text || '').trim()) return null;
    var button = document.createElement('button');
    button.type = 'button';
    button.className = 'channel-quote-button';
    button.title = 'Quote in reply';
    button.setAttribute('aria-label', 'Quote ' + authorText + ' in reply');
    button.innerHTML = '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="9 17 4 12 9 7"></polyline><path d="M20 18v-2a4 4 0 0 0-4-4H4"></path></svg>';
    button.addEventListener('click', function() {
        _channelsInsertQuote(item, authorText);
    });
    return button;
}

function _channelsIsPresenceEvent(entry) {
    if (!entry || entry.hubNotice || !entry.item || entry.item.ours) return false;
    return entry.item.kind === 'join' || entry.item.kind === 'part';
}

function _channelsPresenceIdentityKey(item) {
    var sourceHash = String(item && item.source_hash || '').trim().toLowerCase();
    if (sourceHash) return 'source:' + sourceHash;
    var nickname = String(item && item.nickname || '').trim().toLowerCase();
    return nickname ? 'nickname:' + nickname : '';
}

function _channelsCollapseTransientRejoins(entries) {
    var reconciled = [];
    entries.forEach(function(entry) {
        var previous = reconciled.length ? reconciled[reconciled.length - 1] : null;
        if (_channelsIsPresenceEvent(previous) &&
            _channelsIsPresenceEvent(entry) &&
            previous.item.kind === 'part' &&
            entry.item.kind === 'join') {
            var previousIdentity = _channelsPresenceIdentityKey(previous.item);
            var currentIdentity = _channelsPresenceIdentityKey(entry.item);
            var elapsed = _channelsActivityTime(entry.item) -
                _channelsActivityTime(previous.item);
            if (previousIdentity &&
                previousIdentity === currentIdentity &&
                elapsed >= 0 &&
                elapsed <= CHANNEL_PRESENCE_REJOIN_WINDOW_MS) {
                reconciled.pop();
                return;
            }
        }
        reconciled.push(entry);
    });
    return reconciled;
}

function _channelsGroupPresenceEvents(entries, roomName) {
    var rendered = [];
    var run = [];

    function flushRun() {
        if (!run.length) return;
        if (run.length === 1) {
            rendered.push(run[0]);
        } else {
            var first = run[0].item;
            rendered.push({
                presenceGroup: {
                    key: roomName + '|presence|' + (first.id || first.timestamp_ms || run[0].order),
                    entries: run.slice()
                }
            });
        }
        run = [];
    }

    _channelsCollapseTransientRejoins(entries).forEach(function(entry) {
        if (!_channelsIsPresenceEvent(entry)) {
            flushRun();
            rendered.push(entry);
            return;
        }
        var previous = run.length ? run[run.length - 1].item : null;
        var timestamp = _channelsActivityTime(entry.item);
        var previousTimestamp = previous ? _channelsActivityTime(previous) : timestamp;
        var elapsed = timestamp - previousTimestamp;
        var sameRun = previous &&
            _channelsDayKey(previous) === _channelsDayKey(entry.item) &&
            elapsed >= 0 &&
            elapsed <= CHANNEL_PRESENCE_GROUP_WINDOW_MS;
        if (previous && !sameRun) flushRun();
        run.push(entry);
    });
    flushRun();
    return rendered;
}

function _channelsPresenceGroupSummary(group) {
    var joined = [];
    var left = [];
    group.entries.forEach(function(entry) {
        if (entry.item.kind === 'part') left.push(entry);
        else joined.push(entry);
    });

    var joinedNoun = joined.length === 1 ? 'person' : 'people';
    var leftNoun = left.length === 1 ? 'person' : 'people';
    var text;
    if (joined.length && left.length) {
        text = joined.length + ' ' + joinedNoun + ' joined and ' + left.length + ' left';
    } else if (left.length) {
        text = left.length + ' ' + leftNoun + ' left';
    } else {
        text = joined.length + ' ' + joinedNoun + ' joined';
    }
    return { joined: joined, left: left, text: text };
}

function _channelsPresenceName(item) {
    if (item.nickname) return item.nickname;
    if (item.source_hash) return _channelsShortHash(item.source_hash);
    var text = String(item.text || '');
    var suffix = item.kind === 'part' ? ' left' : ' joined';
    return text.slice(-suffix.length) === suffix ? text.slice(0, -suffix.length) : 'A member';
}

function _channelsPresenceTooltip(entries) {
    var names = entries.map(function(entry) {
        var item = entry.item;
        return item.nickname || item.source_hash || _channelsPresenceName(item);
    });
    var visible = names.slice(0, 8);
    var tooltip = visible.join(', ');
    if (names.length > visible.length) {
        tooltip += ' and ' + (names.length - visible.length) + ' more';
    }
    return tooltip;
}

function _channelsAppendPresenceCount(label, entries, kind, includeNoun) {
    var count = entries.length;
    var noun = count === 1 ? 'person' : 'people';
    var action = kind === 'part' ? 'left' : 'joined';
    var countLabel = count + ' ' + noun + ' ' + action;
    var countElement = document.createElement('span');
    countElement.className = 'channel-presence-count';
    countElement.textContent = String(count);
    countElement.title = _channelsPresenceTooltip(entries);
    countElement.setAttribute('aria-label', countLabel + ': ' + countElement.title);
    label.appendChild(countElement);
    label.appendChild(document.createTextNode(includeNoun ? ' ' + noun + ' ' + action : ' ' + action));
}

function _channelsBuildPresenceGroup(group) {
    var expanded = !!_channelsExpandedPresenceGroups[group.key];
    var summary = _channelsPresenceGroupSummary(group);
    var wrapper = document.createElement('div');
    wrapper.className = 'channel-presence-group' + (expanded ? ' expanded' : '');

    var toggle = document.createElement('button');
    toggle.type = 'button';
    toggle.className = 'channel-presence-summary';
    toggle.setAttribute('aria-expanded', expanded ? 'true' : 'false');
    var listId = 'channel-presence-group-' + (++_channelsPresenceGroupSeq);
    toggle.setAttribute('aria-controls', listId);
    toggle.setAttribute('aria-label', summary.text + '. Show names');

    var label = document.createElement('span');
    label.className = 'channel-presence-label';
    if (summary.joined.length && summary.left.length) {
        _channelsAppendPresenceCount(label, summary.joined, 'join', true);
        label.appendChild(document.createTextNode(' and '));
        _channelsAppendPresenceCount(label, summary.left, 'part', false);
    } else if (summary.left.length) {
        _channelsAppendPresenceCount(label, summary.left, 'part', true);
    } else {
        _channelsAppendPresenceCount(label, summary.joined, 'join', true);
    }
    var chevron = document.createElement('span');
    chevron.className = 'channel-presence-chevron';
    chevron.setAttribute('aria-hidden', 'true');
    chevron.innerHTML = '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="6 9 12 15 18 9"></polyline></svg>';
    toggle.appendChild(label);
    toggle.appendChild(chevron);

    var list = document.createElement('div');
    list.id = listId;
    list.className = 'channel-presence-list';
    list.setAttribute('role', 'list');
    list.hidden = !expanded;
    group.entries.forEach(function(entry) {
        var item = entry.item;
        var nameText = _channelsPresenceName(item);
        var row = document.createElement('div');
        row.className = 'channel-presence-item';
        row.setAttribute('role', 'listitem');
        row.dataset.tone = _channelsIdentityTone(item.source_hash || nameText);
        var marker = document.createElement('span');
        marker.className = 'channel-identity-marker';
        marker.setAttribute('aria-hidden', 'true');
        var copy = document.createElement('span');
        copy.className = 'channel-presence-item-copy';
        copy.textContent = nameText + (item.kind === 'part' ? ' left' : ' joined');
        var time = document.createElement('time');
        time.className = 'channel-event-time';
        time.dateTime = new Date(Number(item.timestamp_ms) || Date.now()).toISOString();
        time.textContent = _channelsFormatTime(item.timestamp_ms);
        row.appendChild(marker);
        row.appendChild(copy);
        row.appendChild(time);
        list.appendChild(row);
    });

    toggle.addEventListener('click', function() {
        expanded = !expanded;
        _channelsExpandedPresenceGroups[group.key] = expanded;
        toggle.setAttribute('aria-expanded', expanded ? 'true' : 'false');
        toggle.setAttribute('aria-label', summary.text + (expanded ? '. Hide names' : '. Show names'));
        wrapper.classList.toggle('expanded', expanded);
        list.hidden = !expanded;
    });
    wrapper.appendChild(toggle);
    wrapper.appendChild(list);
    return wrapper;
}

function _channelsBuildTranscriptItem(item, hubNotice) {
    var kind = item.kind || 'message';
    if (kind === 'join' || kind === 'part' || kind === 'error' || kind === 'system') {
        var system = document.createElement('div');
        system.className = 'channel-system-event' + (kind === 'error' ? ' error' : '');
        system.textContent = item.text || '';
        return system;
    }
    if (hubNotice || _channelsIsHubNotice(item)) return _channelsBuildHubNotice(item);

    var event = document.createElement('article');
    var mentioned = !!item.mentioned && !item.ours;
    event.className = 'channel-event ' + kind +
        (item.ours ? ' ours' : '') + (mentioned ? ' mentioned' : '');
    var authorText = item.nickname || (item.ours ? (channelsSnapshot.nickname || 'You') : _channelsShortHash(item.source_hash)) || 'Hub';
    event.dataset.tone = item.ours ? 'self' : _channelsIdentityTone(item.source_hash || authorText);

    var author = document.createElement('span');
    author.className = 'channel-event-author';
    var marker = document.createElement('i');
    marker.className = 'channel-identity-marker';
    marker.setAttribute('aria-hidden', 'true');
    var authorLabel = document.createElement('span');
    authorLabel.textContent = item.ours ? authorText + ' (you)' : authorText;
    author.appendChild(marker);
    author.appendChild(authorLabel);
    if (mentioned) {
        var mentionMarker = document.createElement('span');
        mentionMarker.className = 'channel-mention-marker';
        mentionMarker.textContent = 'Mention';
        author.appendChild(mentionMarker);
    }
    var time = document.createElement('time');
    time.className = 'channel-event-time';
    time.dateTime = new Date(Number(item.timestamp_ms) || Date.now()).toISOString();
    time.textContent = _channelsFormatTime(item.timestamp_ms);
    var meta = document.createElement('span');
    meta.className = 'channel-event-meta';
    meta.appendChild(time);
    var quote = _channelsBuildQuoteButton(item, authorText);
    if (quote) meta.appendChild(quote);
    var body = document.createElement('div');
    body.className = 'channel-event-text';
    body.textContent = kind === 'action' ? authorText + ' ' + (item.text || '') : (item.text || '');

    event.appendChild(author);
    event.appendChild(meta);
    event.appendChild(body);
    return event;
}

function _channelsMemberName(member) {
    return member.nickname || _channelsShortHash(member.identity_hash) || 'Channel member';
}

function _channelsMemberKey(member) {
    var identity = String(member.identity_hash || '').toLowerCase();
    if (identity) return 'identity:' + identity;
    return 'nickname:' + String(member.nickname || 'channel-member').toLowerCase();
}

function _channelsMemberByKey(members, key) {
    for (var i = 0; i < members.length; i++) {
        if (_channelsMemberKey(members[i]) === key) return members[i];
    }
    return null;
}

function _channelsPeerForIdentity(identityHash) {
    var target = String(identityHash || '').toLowerCase();
    if (!target || typeof PeersCache === 'undefined' || !PeersCache || typeof PeersCache.enriched !== 'function') return null;
    var peers = PeersCache.enriched();
    for (var i = 0; i < peers.length; i++) {
        if (String(peers[i].identity_hash || '').toLowerCase() === target) return peers[i];
    }
    return null;
}

function _channelsPeerLxmfAddress(peer) {
    if (!peer || !peer.hash) return '';
    var services = Array.isArray(peer.services) ? peer.services : [];
    return services.indexOf('lxmf.delivery') !== -1 ? peer.hash : '';
}

function _channelsMemberDetails(member) {
    var identityHash = String(member.identity_hash || '').toLowerCase();
    var peer = member.is_self ? null : _channelsPeerForIdentity(identityHash);
    var active = member.is_self && typeof activeIdentity === 'function' ? activeIdentity() : null;
    var activeMatches = active && (!identityHash || String(active.hash || '').toLowerCase() === identityHash);
    var liveSelf = member.is_self && typeof lxmfIdentity !== 'undefined' ? lxmfIdentity : null;
    var liveSelfMatches = liveSelf && (!identityHash || String(liveSelf.identity_hash || '').toLowerCase() === identityHash);
    var lxmfAddress = member.is_self
        ? String((activeMatches ? active.lxmf_hash : '') || (liveSelfMatches ? liveSelf.hash : '') || '')
        : _channelsPeerLxmfAddress(peer);
    var knownName = member.is_self
        ? String((activeMatches ? (active.display_name || active.nickname) : '') || (liveSelfMatches ? liveSelf.display_name : '') || '')
        : String(peer && peer.display_name || '');
    if (peer && knownName === peer.hash) knownName = '';
    var profileStatus = member.is_self && liveSelfMatches
        ? String(liveSelf.status || liveSelf.profile_status || '')
        : String(peer && peer.profile_status || '');
    return {
        identityHash: identityHash,
        lxmfAddress: lxmfAddress,
        knownName: knownName,
        profileStatus: profileStatus,
        peer: peer,
        active: activeMatches ? active : null
    };
}

function _channelsMemberDetailField(labelText, value, copyLabel) {
    var row = document.createElement('div');
    row.className = 'channel-room-detail channel-member-detail-field';
    if (copyLabel) row.classList.add('copyable');
    var label = document.createElement('span');
    label.textContent = labelText;
    var valueWrap = document.createElement('span');
    valueWrap.className = 'channel-member-detail-value';
    var copy = document.createElement('strong');
    copy.textContent = value;
    if (labelText === 'Identity hash' || labelText === 'LXMF address') copy.classList.add('mono');
    valueWrap.appendChild(copy);
    if (copyLabel) {
        var copyButton = document.createElement('button');
        copyButton.type = 'button';
        copyButton.className = 'channel-member-copy-button';
        copyButton.title = 'Copy ' + copyLabel.toLowerCase();
        copyButton.setAttribute('aria-label', 'Copy ' + copyLabel.toLowerCase());
        copyButton.innerHTML = '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2"></rect><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"></path></svg>';
        copyButton.addEventListener('click', function() {
            RS.copyText(value).then(function(ok) {
                if (ok && typeof showCopyConfirmationToast === 'function') showCopyConfirmationToast(copyLabel);
            });
        });
        valueWrap.appendChild(copyButton);
    }
    row.appendChild(label);
    row.appendChild(valueWrap);
    return row;
}

function _channelsRenderMemberDetail(room, member, list, note) {
    var pane = _channelsEl('channel-members-pane');
    var back = _channelsEl('channel-members-back');
    var details = _channelsMemberDetails(member);
    var channelName = _channelsMemberName(member);
    if (pane) pane.classList.add('showing-detail');
    if (back) back.hidden = false;
    _channelsSetText('channel-members-label', 'People here');
    _channelsSetText('channel-members-count', channelName);
    if (note) note.hidden = true;
    list.classList.add('showing-detail');

    var hero = document.createElement('div');
    hero.className = 'channel-member-detail-hero';
    var avatar = document.createElement('div');
    avatar.className = 'channel-member-detail-avatar';
    if (typeof identityAvatar === 'function') {
        avatar.innerHTML = identityAvatar(details.lxmfAddress || details.identityHash || channelName, 52);
    } else {
        var fallback = document.createElement('span');
        fallback.className = 'channel-identity-marker';
        avatar.appendChild(fallback);
    }
    var heroCopy = document.createElement('div');
    heroCopy.className = 'channel-member-detail-hero-copy';
    var name = document.createElement('strong');
    name.textContent = channelName;
    var presence = document.createElement('span');
    presence.textContent = 'Visible in ' + room.name;
    heroCopy.appendChild(name);
    heroCopy.appendChild(presence);
    if (member.is_self) {
        var you = document.createElement('span');
        you.className = 'channel-member-you';
        you.textContent = 'You';
        heroCopy.appendChild(you);
    }
    hero.appendChild(avatar);
    hero.appendChild(heroCopy);
    list.appendChild(hero);

    var fields = document.createElement('div');
    fields.className = 'channel-room-details channel-member-detail-fields';
    fields.appendChild(_channelsMemberDetailField('Channel name', channelName));
    if (details.knownName && details.knownName.toLowerCase() !== channelName.toLowerCase()) {
        fields.appendChild(_channelsMemberDetailField('Known as', details.knownName));
    }
    if (details.identityHash) {
        fields.appendChild(_channelsMemberDetailField('Identity hash', details.identityHash, 'Identity hash'));
    }
    if (details.lxmfAddress) {
        fields.appendChild(_channelsMemberDetailField('LXMF address', details.lxmfAddress, 'LXMF address'));
    }
    if (details.profileStatus) {
        fields.appendChild(_channelsMemberDetailField('Status', details.profileStatus));
    }
    if (details.peer && details.peer.last_seen != null) {
        var lastHeard = typeof formatLastHeard === 'function'
            ? formatLastHeard(details.peer.last_seen)
            : new Date(details.peer.last_seen * 1000).toLocaleString();
        fields.appendChild(_channelsMemberDetailField('Last heard', lastHeard));
    }
    if (details.peer && details.peer.route_label) {
        fields.appendChild(_channelsMemberDetailField('Route', details.peer.route_label));
    }
    if (details.peer) {
        fields.appendChild(_channelsMemberDetailField('Saved contact', details.peer.is_contact ? 'Yes' : 'No'));
    }
    list.appendChild(fields);

    if (!details.identityHash || !details.lxmfAddress) {
        var hint = document.createElement('p');
        hint.className = 'channel-member-detail-note';
        hint.textContent = !details.identityHash
            ? 'This hub has supplied only a channel nickname for this person.'
            : 'No known LXMF address for this identity yet.';
        list.appendChild(hint);
    }

    if (!member.is_self) {
        var actions = document.createElement('div');
        actions.className = 'channel-member-detail-actions entity-action-grid';
        var mention = document.createElement('button');
        mention.type = 'button';
        mention.className = 'nr-btn entity-action-btn';
        mention.disabled = !_channelsCanCompose();
        mention.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="4"></circle><path d="M16 8v5a3 3 0 0 0 6 0v-1a10 10 0 1 0-3.9 7.9"></path></svg><span>Mention</span>';
        mention.addEventListener('click', function() {
            if (_channelsInsertMemberMention(member)) channelsCloseMemberPane();
        });
        actions.appendChild(mention);
        if (details.lxmfAddress && typeof openConversationWith === 'function') {
            var message = document.createElement('button');
            message.type = 'button';
            message.className = 'nr-btn entity-action-btn';
            message.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"></path></svg><span>Message</span>';
            message.addEventListener('click', function() {
                channelsCloseMemberPane();
                openConversationWith(details.lxmfAddress);
            });
            actions.appendChild(message);
        }
        list.appendChild(actions);
    }
}

function _channelsShowMemberList() {
    var focusKey = _channelsMemberReturnFocusKey;
    _channelsSelectedMemberKey = null;
    _channelsMemberReturnFocusKey = null;
    _channelsRenderMembers(channelsActiveRoom ? _channelsRoomByName(channelsActiveRoom) : null);
    if (focusKey) requestAnimationFrame(function() {
        var rows = document.querySelectorAll('.channel-member-row');
        for (var i = 0; i < rows.length; i++) {
            if (rows[i].dataset.memberKey === focusKey) {
                rows[i].focus();
                break;
            }
        }
    });
}

function _channelsRenderMembers(room) {
    var list = _channelsEl('channel-members-list');
    var note = _channelsEl('channel-members-note');
    var pane = _channelsEl('channel-members-pane');
    var back = _channelsEl('channel-members-back');
    if (!list) return;
    list.textContent = '';
    list.classList.remove('showing-detail');
    if (pane) pane.classList.remove('showing-detail');
    if (back) back.hidden = true;
    if (note) note.hidden = false;
    var members = room && Array.isArray(room.members) ? room.members : [];
    var selectedMember = _channelsSelectedMemberKey
        ? _channelsMemberByKey(members, _channelsSelectedMemberKey)
        : null;
    if (_channelsSelectedMemberKey && !selectedMember) {
        _channelsSelectedMemberKey = null;
        _channelsMemberReturnFocusKey = null;
    }
    if (room && selectedMember) {
        _channelsRenderMemberDetail(room, selectedMember, list, note);
        return;
    }

    _channelsSetText('channel-members-label', 'People here');
    _channelsSetText('channel-members-count', members.length + ' visible');
    if (room && room.phase !== 'joined') {
        if (note) note.textContent = 'Member details appear after the hub confirms your join.';
        var waiting = document.createElement('div');
        waiting.className = 'channel-members-empty';
        waiting.textContent = 'Waiting for channel membership.';
        list.appendChild(waiting);
        return;
    }
    if (note) {
        note.textContent = room && room.members_complete
            ? 'Member list supplied by this hub.'
            : 'The hub may provide only part of the member list.';
    }
    if (!members.length) {
        var empty = document.createElement('div');
        empty.className = 'channel-members-empty';
        empty.textContent = room ? 'No member details have been supplied by this hub yet.' : 'Join a channel to see the people the hub reports.';
        list.appendChild(empty);
        return;
    }
    members.forEach(function(member) {
        var memberKey = _channelsMemberKey(member);
        var nameText = _channelsMemberName(member);
        var row = document.createElement('button');
        row.type = 'button';
        row.className = 'channel-member-row';
        row.dataset.memberKey = memberKey;
        row.dataset.tone = member.is_self ? 'self' : _channelsIdentityTone(member.identity_hash || member.nickname);
        row.setAttribute('aria-label', 'View details for ' + nameText);
        var marker = document.createElement('span');
        marker.className = 'channel-identity-marker';
        marker.setAttribute('aria-hidden', 'true');
        var copy = document.createElement('span');
        copy.className = 'channel-member-copy';
        var name = document.createElement('span');
        name.className = 'channel-member-name';
        name.textContent = nameText;
        copy.appendChild(name);
        if (member.is_self) {
            var you = document.createElement('span');
            you.className = 'channel-member-you';
            you.textContent = 'You';
            copy.appendChild(you);
        }
        var disclosure = document.createElement('span');
        disclosure.className = 'channel-member-disclosure';
        disclosure.setAttribute('aria-hidden', 'true');
        disclosure.innerHTML = '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="9 18 15 12 9 6"></polyline></svg>';
        row.appendChild(marker);
        row.appendChild(copy);
        row.appendChild(disclosure);
        row.addEventListener('click', function() {
            _channelsSelectedMemberKey = memberKey;
            _channelsMemberReturnFocusKey = memberKey;
            _channelsRenderMembers(room);
            requestAnimationFrame(function() {
                var backButton = _channelsEl('channel-members-back');
                if (backButton) backButton.focus();
            });
            if (typeof PeersCache !== 'undefined' && PeersCache &&
                    typeof PeersCache.isInitialized === 'function' && !PeersCache.isInitialized() &&
                    typeof PeersCache.init === 'function') {
                PeersCache.init().then(function() {
                    if (_channelsSelectedMemberKey === memberKey) _channelsRenderMembers(room);
                }).catch(function() {});
            }
        });
        list.appendChild(row);
    });
}

function _channelsUpdateMobileMode() {
    document.querySelectorAll('[data-message-mode]').forEach(function(button) {
        var active = (currentView === 'channels' && button.dataset.messageMode === 'channels') ||
            (currentView === 'message' && button.dataset.messageMode === 'direct');
        button.classList.toggle('active', active);
        button.setAttribute('aria-selected', active ? 'true' : 'false');
    });
}

function _channelsUpdateComposer() {
    var input = _channelsEl('channel-message-input');
    var count = _channelsEl('channel-char-count');
    var send = _channelsEl('channel-send-btn');
    if (!input || !count || !send) return;
    var limit = _channelsMessageLimit();
    var used = _channelsUtf8Length(_channelsMessageBody(input.value));
    count.textContent = used >= Math.floor(limit * 0.75) ? used + '/' + limit : '';
    count.classList.toggle('near-limit', used >= Math.floor(limit * 0.75) && used <= limit);
    count.classList.toggle('over-limit', used > limit);
    var room = channelsActiveRoom ? _channelsRoomByName(channelsActiveRoom) : null;
    send.disabled = _channelsSendPending || !room || room.phase !== 'joined' || channelsSnapshot.phase !== 'active' || !input.value.trim() || used > limit;
    input.placeholder = channelsSnapshot.phase === 'stale' ? 'Connection recovering\u2026' : 'Message channel\u2026';
    input.disabled = !room || room.phase !== 'joined' || channelsSnapshot.phase !== 'active';
}

function _channelsUsesNativeMobileTyping() {
    if (typeof isTauriMobile === 'function' && isTauriMobile()) return true;
    if (typeof isIOS === 'function' && isIOS()) return true;
    return typeof isAndroid === 'function' && isAndroid();
}

function _channelsApplyComposerTypingPolicy(input, useMobileDefaults) {
    if (!input) return;
    var assistanceAttributes = [
        'autocomplete',
        'autocorrect',
        'autocapitalize',
        'spellcheck',
        'writingsuggestions'
    ];
    if (useMobileDefaults) {
        assistanceAttributes.forEach(function(attribute) {
            input.removeAttribute(attribute);
        });
        return;
    }
    input.setAttribute('autocomplete', 'off');
    input.setAttribute('autocorrect', 'off');
    input.setAttribute('autocapitalize', 'off');
    input.setAttribute('spellcheck', 'false');
    input.setAttribute('writingsuggestions', 'false');
}

function _channelsHandleComposerBeforeInput(event, useMobileDefaults) {
    if (useMobileDefaults || !event || event.inputType !== 'insertReplacementText') return;
    event.preventDefault();
}

function channelsSelectRoom(roomName) {
    var room = _channelsRoomByName(roomName);
    if (!room) return;
    if (channelsActiveRoom !== room.name || channelsHistorySelection) {
        _channelsSelectedMemberKey = null;
        _channelsMemberReturnFocusKey = null;
    }
    channelsHistorySelection = null;
    channelsActiveRoom = room.name;
    renderChannels();
    if (_channelsCompact() && RS.viewStack) {
        var top = RS.viewStack.top();
        if (!top || top.viewId !== 'channel-detail') {
            RS.viewStack.push('channel-detail', { room: room.name });
        }
    }
    setTimeout(function() { channelsPrepareVisibleRead(); }, 0);
}

function channelsSelectHistoryRoom(hubDestinationHash, roomName) {
    var key = _channelsHistoryKey(hubDestinationHash, roomName);
    if (!key) return;
    _channelsSelectedMemberKey = null;
    _channelsMemberReturnFocusKey = null;
    channelsActiveRoom = null;
    channelsHistorySelection = {
        hub_destination_hash: String(hubDestinationHash).toLowerCase(),
        room_name: String(roomName).toLowerCase()
    };
    channelsCloseMemberPane();
    renderChannels();
    if (_channelsCompact() && RS.viewStack) {
        var top = RS.viewStack.top();
        if (!top || top.viewId !== 'channel-detail') {
            RS.viewStack.push('channel-detail', {
                room: channelsHistorySelection.room_name,
                history: true
            });
        }
    }
    setTimeout(function() { channelsPrepareVisibleRead(); }, 0);
}

function channelsOpenNotificationRoute(hubDestinationHash, roomName) {
    var hub = String(hubDestinationHash || '').trim().toLowerCase();
    var room = String(roomName || '').trim().toLowerCase();
    if (!/^[0-9a-f]{32}$/.test(hub) || !room ||
            _channelsUtf8Length(room) > 256 ||
            /[\u0000-\u001f\u007f]/.test(room)) return Promise.resolve(false);
    if (typeof switchView === 'function') switchView('channels');
    var load = typeof channelsLoad === 'function'
        ? channelsLoad(true)
        : Promise.resolve(channelsSnapshot);
    return Promise.resolve(load).then(function() {
        var activeHub = channelsSnapshot.hub &&
            String(channelsSnapshot.hub.destination_hash || '').toLowerCase();
        if (activeHub === hub && _channelsRoomByName(room)) {
            channelsSelectRoom(room);
        } else {
            // Routes intentionally carry no room key and never initiate a
            // connection. The bounded local timeline remains safe to open
            // while its hub is offline or no longer bookmarked.
            channelsSelectHistoryRoom(hub, room);
        }
        return true;
    });
}
window.channelsOpenNotificationRoute = channelsOpenNotificationRoute;

function _onChannelDetailExit() {
    channelsCloseMemberPane();
    var input = _channelsEl('channel-message-input');
    if (input) input.blur();
}

function channelsCloseMemberPane() {
    _channelsSelectedMemberKey = null;
    _channelsMemberReturnFocusKey = null;
    _channelsRenderMembers(channelsActiveRoom ? _channelsRoomByName(channelsActiveRoom) : null);
    var layout = _channelsEl('channels-layout');
    if (layout) layout.classList.remove('members-open');
}

function _channelsSafeShareFileName(roomName) {
    var room = String(roomName || 'hub').toLowerCase()
        .replace(/[^a-z0-9]+/g, '-')
        .replace(/^-+|-+$/g, '')
        .slice(0, 40);
    return 'ratspeak-channel-' + (room || 'hub') + '.png';
}

function channelsOpenChannelShare(hubDestinationHash, roomName) {
    if (typeof _rsBuildSheet !== 'function') return;
    RS.invoke('api_channel_share', {
        args: {
            hub_destination_hash: hubDestinationHash,
            room: roomName || null
        }
    }).then(function(target) {
        var built = _rsBuildSheet({
            title: target.room ? 'Share Channel' : 'Share Hub'
        }, function() {});
        var copy = document.createElement('p');
        copy.className = 'channel-sheet-copy';
        copy.textContent = target.room
            ? 'Share ' + target.room + ' on this hub.'
            : 'Share this channel hub.';
        built.body.appendChild(copy);

        var details = document.createElement('div');
        details.className = 'channel-room-details channel-share-details';
        details.appendChild(_channelsRoomDetail(
            'Hub',
            _channelsTimelineHubName(_channelsHubByDestination(
                target.hub_destination_hash
            ))
        ));
        details.appendChild(_channelsRoomDetail(
            'Destination',
            _channelsShortHash(target.hub_destination_hash)
        ));
        if (target.room) {
            details.appendChild(_channelsRoomDetail('Channel', target.room));
        }
        built.body.appendChild(details);

        var canvasShell = document.createElement('div');
        canvasShell.className = 'channel-share-qr-shell';
        var canvas = document.createElement('canvas');
        canvas.className = 'channel-share-qr';
        canvas.setAttribute('aria-label', 'Ratspeak channel share QR');
        canvasShell.appendChild(canvas);
        built.body.appendChild(canvasShell);
        var qrReady = !!(RS.qr && typeof RS.qr.renderCanvas === 'function');
        if (qrReady) {
            try {
                RS.qr.renderCanvas(canvas, target.payload);
            } catch (_) {
                qrReady = false;
                canvasShell.hidden = true;
            }
        } else {
            canvasShell.hidden = true;
        }

        var actions = document.createElement('div');
        actions.className = 'channel-share-actions';
        var copyLink = document.createElement('button');
        copyLink.type = 'button';
        copyLink.className = 'nr-btn nr-btn-secondary';
        copyLink.textContent = 'Copy link';
        copyLink.addEventListener('click', function() {
            RS.copyText(target.payload).then(function(ok) {
                if (typeof showToast === 'function') {
                    showToast(
                        ok ? 'Channel link copied' : 'Could not copy channel link',
                        ok ? 'toast-green' : 'toast-orange',
                        2200
                    );
                }
            });
        });
        actions.appendChild(copyLink);
        var shareQr = document.createElement('button');
        shareQr.type = 'button';
        shareQr.className = 'nr-btn nr-btn-secondary';
        shareQr.textContent = 'Share QR';
        shareQr.hidden = !qrReady || !RS.qr || typeof RS.qr.shareCanvas !== 'function';
        shareQr.addEventListener('click', function() {
            shareQr.disabled = true;
            RS.qr.shareCanvas(
                canvas,
                _channelsSafeShareFileName(target.room),
                'Ratspeak Channel'
            ).then(function(method) {
                if (typeof showToast === 'function') {
                    showToast(
                        method === 'share' ? 'QR handed to destination' : 'QR image saved',
                        'toast-green',
                        2400
                    );
                }
            }).catch(function(error) {
                if (typeof showToast === 'function') {
                    showToast(
                        (error && error.message) || 'Could not share channel QR',
                        'toast-red',
                        3200
                    );
                }
            }).then(function() {
                shareQr.disabled = false;
            });
        });
        actions.appendChild(shareQr);
        built.body.appendChild(actions);

        var note = document.createElement('div');
        note.className = 'channel-sheet-trust-note';
        note.textContent = 'This share contains only the hub destination and optional channel name. It never includes a channel key or joins on open.';
        built.body.appendChild(note);

        var done = document.createElement('button');
        done.type = 'button';
        done.className = 'nr-btn nr-btn-primary';
        done.textContent = 'Done';
        done.addEventListener('click', built.dismiss);
        built.footer.appendChild(done);
        _channelsPresentSheet(built, copyLink);
    }).catch(function(error) {
        if (typeof showToast === 'function') {
            showToast(
                (error && error.message) || 'Could not build channel share',
                'toast-red',
                3200
            );
        }
    });
}

function _channelsPresentSharedTarget(target) {
    if (!target || typeof _rsBuildSheet !== 'function') return;
    var destination = String(target.hub_destination_hash || '').toLowerCase();
    var hub = _channelsHubByDestination(destination);
    var saved = _channelsSavedHub(destination);
    var discovered = channelsDiscoveredHubs.find(function(candidate) {
        return String(candidate.destination_hash || '').toLowerCase() === destination;
    });
    var activeDestination = channelsSnapshot.hub &&
        String(channelsSnapshot.hub.destination_hash || '').toLowerCase();
    var sameHub = _channelsIsConnected() && activeDestination === destination;
    var joinedRoom = sameHub && target.room && _channelsRoomByName(target.room);

    var built = _rsBuildSheet({ title: 'Shared Channel' }, function() {});
    var copy = document.createElement('p');
    copy.className = 'channel-sheet-copy';
    copy.textContent = target.room
        ? 'Someone shared ' + target.room + ' on ' + _channelsTimelineHubName(hub) + '.'
        : 'Someone shared ' + _channelsTimelineHubName(hub) + '.';
    built.body.appendChild(copy);

    var details = document.createElement('div');
    details.className = 'channel-room-details channel-share-details';
    details.appendChild(_channelsRoomDetail('Hub destination', destination));
    if (target.room) details.appendChild(_channelsRoomDetail('Channel', target.room));
    var availability;
    if (sameHub) {
        availability = 'Connected and authenticated now';
    } else if (discovered && typeof discovered.hops === 'number') {
        availability = discovered.hops === 0
            ? 'Heard directly'
            : 'Heard at ' + discovered.hops +
                (discovered.hops === 1 ? ' hop' : ' hops');
    } else if (saved) {
        availability = 'Saved on this device; not currently connected';
    } else {
        availability = 'Not currently heard or saved on this device';
    }
    details.appendChild(_channelsRoomDetail('Availability', availability));
    built.body.appendChild(details);

    var trust = document.createElement('div');
    trust.className = 'channel-sheet-trust-note';
    trust.textContent = 'A share names a destination; it is not proof of who operates it. Ratspeak authenticates the Reticulum Link before any channel action. No channel key is present.';
    built.body.appendChild(trust);

    var cancel = document.createElement('button');
    cancel.type = 'button';
    cancel.className = 'nr-btn nr-btn-secondary';
    cancel.textContent = 'Cancel';
    cancel.addEventListener('click', built.dismiss);
    built.footer.appendChild(cancel);

    var review = document.createElement('button');
    review.type = 'button';
    review.className = 'nr-btn nr-btn-primary';
    if (joinedRoom && joinedRoom.phase === 'joined') {
        review.textContent = 'Open channel';
    } else if (sameHub && target.room) {
        review.textContent = 'Review join';
    } else if (sameHub) {
        review.textContent = 'View hub';
    } else {
        review.textContent = 'Review connection';
    }
    review.addEventListener('click', function() {
        built.dismiss();
        setTimeout(function() {
            if (joinedRoom && joinedRoom.phase === 'joined') {
                channelsSelectRoom(target.room);
            } else if (sameHub && target.room) {
                channelsOpenJoinSheet(target.room);
            } else if (sameHub) {
                channelsOpenHubOptions();
            } else {
                channelsOpenConnectSheet({
                    destination_hash: destination,
                    announced_name: hub.announced_name || hub.label || '',
                    nickname: hub.nickname || '',
                    shared_room: target.room || null
                });
            }
        }, 220);
    });
    built.footer.appendChild(review);
    _channelsPresentSheet(built, review);
}

// Native URI handling receives only the typed result of the canonical Rust
// parser. Keep this entry point preview-only: the sheet can lead the user to a
// separate connection or join review, but opening a URI performs neither.
function channelsOpenNativeSharedChannel(target) {
    if (!target || target.format !== 'ratspeak.channel.v1') return false;
    if (!/^[0-9a-f]{32}$/.test(String(target.hub_destination_hash || ''))) {
        return false;
    }
    if (typeof target.payload !== 'string' || target.payload.length > 230) {
        return false;
    }
    if (target.room != null && typeof target.room !== 'string') return false;
    if (Object.prototype.hasOwnProperty.call(target, 'key') ||
            Object.prototype.hasOwnProperty.call(target, 'join_key')) {
        return false;
    }
    _channelsPresentSharedTarget(target);
    return true;
}
window.channelsOpenNativeSharedChannel = channelsOpenNativeSharedChannel;

function channelsScanSharedChannel() {
    if (!RS.qr || typeof RS.qr.openScanner !== 'function') {
        if (typeof showToast === 'function') {
            showToast('QR scanning is not available in this build', 'toast-orange', 2800);
        }
        return;
    }
    RS.qr.openScanner({
        title: 'Scan Channel QR',
        checkingText: 'Checking channel share\u2026',
        previewCommand: 'api_preview_channel_share',
        invalidText: 'That QR is not a valid Ratspeak channel share.',
        invalidImageText: 'That image does not contain a valid Ratspeak channel QR.',
        emptyImageText: 'No Ratspeak channel QR found in that image.',
        onPreview: function(_body, _payload, target, closeAll) {
            closeAll();
            setTimeout(function() { _channelsPresentSharedTarget(target); }, 220);
        }
    });
}

function channelsOpenSharedChannel(payload) {
    if (typeof _rsBuildSheet !== 'function') return;
    var built = _rsBuildSheet({ title: 'Open Shared Channel' }, function() {});
    var copy = document.createElement('p');
    copy.className = 'channel-sheet-copy';
    copy.textContent = 'Paste a Ratspeak channel link or scan its QR. Opening it only previews the destination.';
    built.body.appendChild(copy);

    var input = document.createElement('textarea');
    input.className = 'nr-input-sm channel-share-input mono';
    input.rows = 4;
    input.maxLength = 230;
    input.placeholder = 'ratspeak://channel?v=1&hub=\u2026';
    input.autocomplete = 'off';
    input.setAttribute('autocorrect', 'off');
    input.setAttribute('autocapitalize', 'none');
    input.setAttribute('spellcheck', 'false');
    input.setAttribute('writingsuggestions', 'false');
    input.value = payload || '';
    built.body.appendChild(_channelsSheetField('Channel link', input));

    var tools = document.createElement('div');
    tools.className = 'channel-share-actions';
    var scan = document.createElement('button');
    scan.type = 'button';
    scan.className = 'nr-btn nr-btn-secondary';
    scan.textContent = 'Scan QR';
    scan.hidden = !RS.qr || typeof RS.qr.openScanner !== 'function';
    scan.addEventListener('click', function() {
        built.dismiss();
        setTimeout(channelsScanSharedChannel, 220);
    });
    tools.appendChild(scan);
    var paste = document.createElement('button');
    paste.type = 'button';
    paste.className = 'nr-btn nr-btn-secondary';
    paste.textContent = 'Paste';
    paste.hidden = !navigator.clipboard || typeof navigator.clipboard.readText !== 'function';
    paste.addEventListener('click', function() {
        navigator.clipboard.readText().then(function(text) {
            input.value = text || '';
            input.focus();
        }).catch(function() {
            error.textContent = 'Clipboard access was unavailable. Paste the link into the field.';
            input.focus();
        });
    });
    tools.appendChild(paste);
    built.body.appendChild(tools);

    var error = document.createElement('div');
    error.className = 'channel-sheet-error';
    error.setAttribute('aria-live', 'polite');
    built.body.appendChild(error);
    function preview() {
        var raw = input.value.trim();
        if (!raw) {
            error.textContent = 'Paste or scan a Ratspeak channel link.';
            input.focus();
            return;
        }
        open.disabled = true;
        open.textContent = 'Checking\u2026';
        error.textContent = '';
        RS.invoke('api_preview_channel_share', { payload: raw }).then(function(target) {
            built.dismiss();
            setTimeout(function() { _channelsPresentSharedTarget(target); }, 220);
        }).catch(function(previewError) {
            error.textContent = (previewError && previewError.message) ||
                'That is not a valid Ratspeak channel share.';
            open.disabled = false;
            open.textContent = 'Preview';
        });
    }

    var cancel = document.createElement('button');
    cancel.type = 'button';
    cancel.className = 'nr-btn nr-btn-secondary';
    cancel.textContent = 'Cancel';
    cancel.addEventListener('click', built.dismiss);
    var open = document.createElement('button');
    open.type = 'button';
    open.className = 'nr-btn nr-btn-primary';
    open.textContent = 'Preview';
    open.addEventListener('click', preview);
    built.footer.appendChild(cancel);
    built.footer.appendChild(open);
    _channelsPresentSheet(built, input);
    if (payload) setTimeout(preview, 0);
}
window.channelsOpenSharedChannel = channelsOpenSharedChannel;

function _channelsHubCurrentStatus() {
    switch (channelsSnapshot.phase) {
        case 'active': return { label: 'Live now', tone: 'joined' };
        case 'stale': return { label: 'Recovering', tone: 'recovering' };
        case 'reconnecting': return { label: 'Reconnecting', tone: 'recovering' };
        case 'error': return { label: 'Needs attention', tone: 'error' };
        default: return { label: _channelsPhaseLabel(channelsSnapshot.phase), tone: '' };
    }
}

function channelsOpenHubSwitcher() {
    if (typeof _rsBuildSheet !== 'function') return;
    if (typeof _channelsHubSwitcherDismiss === 'function') {
        _channelsHubSwitcherDismiss();
    }
    var openedEpoch = _channelsHistoryEpoch;
    var openedGeneration = Number(channelsSnapshot.generation) || 0;
    var built = _rsBuildSheet({ title: 'Channel hubs' }, function() {
        if (_channelsHubSwitcherDismiss === built.dismiss) {
            _channelsHubSwitcherDismiss = null;
        }
    });
    _channelsHubSwitcherDismiss = built.dismiss;
    built.sheet.classList.add('channel-hub-switcher-sheet');

    var copy = document.createElement('p');
    copy.className = 'channel-sheet-copy';
    copy.textContent = 'Ratspeak keeps one live hub at a time. Switching ends the current live rooms; saved channels and local history stay on this device.';
    built.body.appendChild(copy);

    var list = document.createElement('div');
    list.className = 'channel-hub-switcher-list';
    list.setAttribute('aria-live', 'polite');
    built.body.appendChild(list);
    var refreshInFlight = false;

    function contextIsCurrent() {
        return openedEpoch === _channelsHistoryEpoch &&
            openedGeneration === (Number(channelsSnapshot.generation) || 0);
    }

    function retireStaleSwitcher() {
        built.dismiss();
        if (typeof showToast === 'function') {
            showToast('Channels changed. Choose a hub again.', 'toast-orange', 2800);
        }
    }

    function chooseHub(hub, current) {
        if (!contextIsCurrent()) {
            retireStaleSwitcher();
            return;
        }
        built.dismiss();
        setTimeout(function() {
            if (!contextIsCurrent()) {
                retireStaleSwitcher();
                return;
            }
            if (current && channelsSnapshot.hub) channelsOpenHubOptions();
            else channelsOpenConnectSheet(hub);
        }, 220);
    }

    function appendHubSection(label, hubs, options) {
        options = options || {};
        if (!hubs.length && !options.always) return;
        var section = _channelsListSection(label, {
            className: 'channel-hub-switcher-section',
            actionText: options.actionText,
            actionDisabled: options.actionDisabled,
            action: options.action
        });
        if (options.refreshAction) {
            var sectionAction = section.querySelector('.channels-list-section-action');
            if (sectionAction) sectionAction.dataset.channelHubRefresh = 'true';
        }
        list.appendChild(section);
        hubs.forEach(function(hub) {
            list.appendChild(_channelsBuildHubRow(hub, {
                current: !!options.current,
                disabled: !!options.disabled,
                status: options.status || '',
                statusTone: options.statusTone || '',
                onSelect: function(selected) {
                    chooseHub(selected, !!options.current);
                }
            }));
        });
    }

    function refreshNearby() {
        if (refreshInFlight) return;
        if (!contextIsCurrent()) {
            retireStaleSwitcher();
            return;
        }
        refreshInFlight = true;
        list.setAttribute('aria-busy', 'true');
        var refreshButton = list.querySelector('[data-channel-hub-refresh]');
        if (refreshButton) {
            refreshButton.disabled = true;
            refreshButton.textContent = 'Checking\u2026';
        }
        channelsRefreshAvailableHubs().then(function() {
            refreshInFlight = false;
            list.removeAttribute('aria-busy');
            if (built.sheet.isConnected && contextIsCurrent()) {
                renderList();
                var nextRefresh = list.querySelector('[data-channel-hub-refresh]');
                if (nextRefresh) nextRefresh.focus();
            } else if (built.sheet.isConnected) {
                retireStaleSwitcher();
            }
        }, function() {
            refreshInFlight = false;
            list.removeAttribute('aria-busy');
            if (built.sheet.isConnected && contextIsCurrent()) {
                renderList();
                var nextRefresh = list.querySelector('[data-channel-hub-refresh]');
                if (nextRefresh) nextRefresh.focus();
            } else if (built.sheet.isConnected) {
                retireStaleSwitcher();
            }
        });
    }

    function renderList() {
        list.textContent = '';
        var model = _channelsHubSwitcherModel();
        var blocked = _channelsConnectCommandBlocked();
        if (model.current) {
            var currentStatus = _channelsHubCurrentStatus();
            appendHubSection('Current selection', [model.current], {
                current: true,
                status: currentStatus.label,
                statusTone: currentStatus.tone
            });
        }
        appendHubSection('Recently heard', model.nearby, {
            always: true,
            actionText: refreshInFlight ? 'Checking\u2026' : 'Refresh',
            actionDisabled: refreshInFlight,
            action: refreshNearby,
            refreshAction: true,
            disabled: blocked
        });
        if (!model.nearby.length) {
            list.appendChild(_channelsDirectoryStatus(
                refreshInFlight ? 'Checking recent hub announcements\u2026' : 'No other hubs heard recently',
                'Saved hubs remain available below'
            ));
        }
        appendHubSection('Saved on this device', model.saved, {
            disabled: blocked
        });
        if (!model.current && !model.nearby.length && !model.saved.length) {
            list.appendChild(_channelsDirectoryStatus(
                'No channel hubs yet',
                'Add a trusted destination address to begin'
            ));
        }
        if (blocked && (model.nearby.length || model.saved.length)) {
            list.appendChild(_channelsDirectoryStatus(
                'Connection in progress',
                'Open the current selection to cancel before choosing another hub',
                'warning'
            ));
        }
    }

    var add = document.createElement('button');
    add.type = 'button';
    add.className = 'nr-btn nr-btn-secondary';
    add.textContent = 'Add by address';
    add.addEventListener('click', function() {
        built.dismiss();
        setTimeout(function() { channelsOpenConnectSheet(); }, 220);
    });
    var done = document.createElement('button');
    done.type = 'button';
    done.className = 'nr-btn nr-btn-primary';
    done.textContent = 'Done';
    done.addEventListener('click', built.dismiss);
    built.footer.appendChild(add);
    built.footer.appendChild(done);

    renderList();
    _channelsPresentSheet(built, modelCurrentFocus());

    function modelCurrentFocus() {
        return list.querySelector('.channel-hub-row.current') ||
            list.querySelector('.channel-hub-row:not(:disabled)') ||
            add;
    }
}

function channelsOpenConnectSheet(prefill) {
    if (typeof _rsBuildSheet !== 'function') return;
    prefill = prefill || {};
    var hubs = _channelsMergedHubs();
    var selectedHash = prefill.destination_hash || (channelsSnapshot.hub && channelsSnapshot.hub.destination_hash) || '';
    var selectedLabel = prefill.label || prefill.announced_name || '';
    var sharedDestination = prefill.shared_room
        ? String(prefill.destination_hash || '').toLowerCase()
        : '';
    var sharedRoom = prefill.shared_room || '';
    var defaultNick = prefill.nickname || (_channelsSavedHub(selectedHash) || {}).nickname || channelsSnapshot.nickname || _channelsDefaultNickname();
    var built = _rsBuildSheet({ title: 'Connect to Channels' }, function() {});
    var titleElement = built.sheet.querySelector('.bottom-sheet-title');

    var copy = document.createElement('p');
    copy.className = 'channel-sheet-copy';
    copy.textContent = sharedRoom
        ? 'This share points to ' + sharedRoom + '. Review and authenticate the hub first; Ratspeak will then open a separate join review.'
        : 'Choose a recently heard hub or enter its destination. Ratspeak will authenticate the connection.';
    built.body.appendChild(copy);

    var openShared = document.createElement('button');
    openShared.type = 'button';
    openShared.className = 'nr-btn nr-btn-secondary channel-open-share-btn';
    openShared.textContent = 'Open a shared channel';
    openShared.addEventListener('click', function() {
        built.dismiss();
        setTimeout(function() { channelsOpenSharedChannel(); }, 220);
    });
    built.body.appendChild(openShared);

    var trust = document.createElement('div');
    trust.className = 'channel-sheet-trust-note';
    trust.innerHTML = '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg><span>Choose a hub you trust. The Link is encrypted in transit, but the hub relays and can read channel messages.</span>';
    built.body.appendChild(trust);

    var switchImpact = document.createElement('div');
    switchImpact.className = 'channel-hub-switch-impact';
    switchImpact.hidden = true;
    built.body.appendChild(switchImpact);

    var destinationInput = document.createElement('input');
    destinationInput.type = 'text';
    destinationInput.className = 'nr-input-sm mono';
    destinationInput.placeholder = '32-character destination hash';
    destinationInput.autocapitalize = 'none';
    destinationInput.autocomplete = 'off';
    destinationInput.spellcheck = false;
    destinationInput.maxLength = 32;
    destinationInput.value = selectedHash;
    destinationInput.addEventListener('input', function() {
        var entered = destinationInput.value.trim().toLowerCase();
        if (entered !== selectedHash) selectedLabel = '';
        if (entered !== sharedDestination) sharedRoom = '';
        updateConnectionChoice();
    });

    if (hubs.length) {
        var availableLabel = document.createElement('div');
        availableLabel.className = 'channels-section-label';
        availableLabel.textContent = 'Available hubs';
        built.body.appendChild(availableLabel);
        var available = document.createElement('div');
        available.className = 'channel-sheet-hubs';
        hubs.forEach(function(hub) {
            var row = document.createElement('button');
            row.type = 'button';
            row.className = 'channel-sheet-hub' + (hub.destination_hash === selectedHash ? ' selected' : '');
            var rowCopy = document.createElement('span');
            rowCopy.className = 'channel-sheet-hub-copy';
            var title = document.createElement('strong');
            title.textContent = _channelsHubName(hub);
            var hash = document.createElement('span');
            hash.textContent = _channelsHubMeta(hub);
            rowCopy.appendChild(title);
            rowCopy.appendChild(hash);
            row.appendChild(_channelsBuildHubMark(hub));
            row.appendChild(rowCopy);
            var distance = document.createElement('span');
            distance.className = 'channel-hub-row-distance';
            distance.textContent = _channelsHubDistance(hub);
            if (distance.textContent) row.appendChild(distance);
            row.addEventListener('click', function() {
                selectedHash = hub.destination_hash;
                selectedLabel = hub.label || hub.announced_name || '';
                sharedRoom = hub.destination_hash === sharedDestination
                    ? prefill.shared_room || ''
                    : '';
                destinationInput.value = selectedHash;
                if (hub.nickname) nicknameInput.value = hub.nickname;
                available.querySelectorAll('.channel-sheet-hub').forEach(function(el) { el.classList.remove('selected'); });
                row.classList.add('selected');
                error.textContent = '';
                updateConnectionChoice();
            });
            available.appendChild(row);
        });
        built.body.appendChild(available);
    }

    var destinationField = _channelsSheetField('Hub destination', destinationInput);
    built.body.appendChild(destinationField);
    var nicknameInput = document.createElement('input');
    nicknameInput.type = 'text';
    nicknameInput.className = 'nr-input-sm';
    nicknameInput.placeholder = 'Nickname for this session';
    nicknameInput.maxLength = 32;
    nicknameInput.value = defaultNick;
    built.body.appendChild(_channelsSheetField('Your channel nickname', nicknameInput));
    var error = document.createElement('div');
    error.className = 'channel-sheet-error';
    error.setAttribute('aria-live', 'polite');
    built.body.appendChild(error);

    var cancel = document.createElement('button');
    cancel.type = 'button';
    cancel.className = 'nr-btn nr-btn-secondary';
    cancel.textContent = 'Cancel';
    cancel.addEventListener('click', function() { built.dismiss(); });
    var connect = document.createElement('button');
    connect.type = 'button';
    connect.className = 'nr-btn nr-btn-primary';
    var connectBusy = false;

    function updateConnectionChoice() {
        var destination = destinationInput.value.trim().toLowerCase();
        var mode = _channelsHubConnectMode(destination);
        var currentHub = mode.current_destination
            ? _channelsHubByDestination(mode.current_destination)
            : null;
        var currentName = currentHub
            ? _channelsTimelineHubName(currentHub)
            : 'the current hub';
        switchImpact.hidden = true;

        if (mode.kind === 'switch') {
            if (titleElement) titleElement.textContent = 'Switch channel hub';
            copy.textContent = sharedRoom
                ? 'This share points to ' + sharedRoom + '. Review and authenticate the new hub first; Ratspeak will then open a separate join review.'
                : 'Review the next hub and nickname before replacing the current live session.';
            switchImpact.hidden = false;
            switchImpact.textContent = channelsSnapshot.phase === 'reconnecting'
                ? 'Switching stops recovery for ' + currentName + '. Saved channels and local history stay on this device.'
                : 'Switching ends your live rooms on ' + currentName + '. Saved channels and local history stay on this device.';
            connect.textContent = connectBusy
                ? 'Switching\u2026'
                : (sharedRoom ? 'Switch and review' : 'Switch hub');
            connect.disabled = connectBusy;
        } else if (mode.kind === 'current') {
            if (titleElement) titleElement.textContent = 'Current channel hub';
            copy.textContent = 'You are already connected to this hub. Choose another destination to switch, or use Hub options to manage this session.';
            connect.textContent = 'Connected';
            connect.disabled = true;
        } else if (mode.kind === 'recovering') {
            if (titleElement) titleElement.textContent = 'Hub recovery in progress';
            copy.textContent = 'Ratspeak is recovering this hub connection. Choose another destination to stop recovery and switch.';
            connect.textContent = 'Recovering\u2026';
            connect.disabled = true;
        } else if (mode.kind === 'pending') {
            if (titleElement) titleElement.textContent = 'Connection in progress';
            copy.textContent = 'Finish or cancel the current connection before choosing another hub.';
            switchImpact.hidden = false;
            switchImpact.textContent = 'Open the current hub from the switcher to cancel this attempt.';
            connect.textContent = 'Connection in progress';
            connect.disabled = true;
        } else {
            if (titleElement) titleElement.textContent = 'Connect to Channels';
            copy.textContent = sharedRoom
                ? 'This share points to ' + sharedRoom + '. Review and authenticate the hub first; Ratspeak will then open a separate join review.'
                : 'Choose a recently heard hub or enter its destination. Ratspeak will authenticate the connection.';
            connect.textContent = connectBusy
                ? 'Connecting\u2026'
                : (sharedRoom ? 'Connect and review' : 'Connect');
            connect.disabled = connectBusy;
        }
    }

    connect.addEventListener('click', function() {
        var destination = destinationInput.value.trim().toLowerCase();
        var nickname = nicknameInput.value.trim();
        var connectMode = _channelsHubConnectMode(destination);
        if (!/^[0-9a-f]{32}$/.test(destination)) {
            error.textContent = 'Enter a 32-character hexadecimal destination hash.';
            destinationInput.focus();
            return;
        }
        if (!nickname) {
            error.textContent = 'Choose a nickname for this session.';
            nicknameInput.focus();
            return;
        }
        connectBusy = true;
        updateConnectionChoice();
        error.textContent = '';
        var pendingShare = sharedRoom && destination === sharedDestination
            ? {
                destination_hash: destination,
                room: sharedRoom,
                generation: Number(channelsSnapshot.generation)
            }
            : null;
        channelsPendingShareJoin = pendingShare;
        channelsConnectToHub({
            destination_hash: destination,
            announced_name: selectedLabel,
            nickname: nickname
        }, {
            preserve_pending_share: true,
            switching: connectMode.kind === 'switch'
        }).then(function() {
            built.dismiss();
        }).catch(function(err) {
            if (channelsPendingShareJoin === pendingShare) {
                channelsPendingShareJoin = null;
            }
            error.textContent = (err && err.message) ||
                (connectMode.kind === 'switch'
                    ? 'Could not switch channel hubs.'
                    : 'Could not connect to this hub.');
            connectBusy = false;
            updateConnectionChoice();
        });
    });
    built.footer.appendChild(cancel);
    built.footer.appendChild(connect);
    updateConnectionChoice();
    _channelsPresentSheet(built, selectedHash ? nicknameInput : destinationInput);
}

function _channelsSheetField(labelText, input) {
    var field = document.createElement('div');
    field.className = 'channel-sheet-field';
    var label = document.createElement('label');
    var inputId = 'channel-sheet-field-' + (++_channelsFieldSeq);
    input.id = inputId;
    label.htmlFor = inputId;
    label.textContent = labelText;
    field.appendChild(label);
    field.appendChild(input);
    return field;
}

function _channelsPresentSheet(built, initialFocus) {
    built.overlay.addEventListener('click', function(event) {
        if (event.target === built.overlay) built.dismiss();
    });
    built.sheet.addEventListener('keydown', function(event) {
        if (event.key === 'Escape') {
            event.stopPropagation();
            built.dismiss();
            return;
        }
        if (event.key === 'Enter' &&
                (event.target.tagName === 'INPUT' || event.target.tagName === 'SELECT')) {
            var primary = built.sheet.querySelector('.nr-btn-primary:not(:disabled)');
            if (primary) {
                event.preventDefault();
                primary.click();
                return;
            }
        }
        if (event.key !== 'Tab') return;
        var focusable = built.sheet.querySelectorAll('input, textarea, select, button:not(:disabled)');
        if (!focusable.length) return;
        var first = focusable[0];
        var last = focusable[focusable.length - 1];
        if (event.shiftKey && document.activeElement === first) {
            event.preventDefault();
            last.focus();
        } else if (!event.shiftKey && document.activeElement === last) {
            event.preventDefault();
            first.focus();
        }
    });
    if (RS.gestures && typeof RS.gestures.attachDragDismiss === 'function') {
        RS.gestures.attachDragDismiss(built.sheet, {
            axis: 'y',
            blockIfScrolled: true,
            skipIf: function(event) {
                return !!event.target.closest('button, input, textarea, select');
            },
            parallaxOverlay: built.overlay,
            onCommit: function() { built.dismiss(); }
        });
    }
    built.present();
    if (initialFocus) setTimeout(function() { initialFocus.focus(); }, 250);
}

function channelsOpenJoinSheet(prefillRoom) {
    if (!_channelsIsConnected() || typeof _rsBuildSheet !== 'function') return;
    var built = _rsBuildSheet({ title: 'Join a Channel' }, function() {});
    var copy = document.createElement('p');
    copy.className = 'channel-sheet-copy';
    copy.textContent = 'Join a channel on ' + _channelsHubName(channelsSnapshot.hub) + '. Names are case-insensitive.';
    built.body.appendChild(copy);

    var roomInput = document.createElement('input');
    roomInput.type = 'text';
    roomInput.className = 'nr-input-sm';
    roomInput.placeholder = 'Channel name';
    roomInput.autocomplete = 'off';
    roomInput.setAttribute('autocorrect', 'off');
    roomInput.setAttribute('autocapitalize', 'none');
    roomInput.setAttribute('spellcheck', 'false');
    roomInput.setAttribute('writingsuggestions', 'false');
    if (typeof disableAutoCorrect === 'function') disableAutoCorrect(roomInput);
    roomInput.maxLength = (channelsSnapshot.hub && channelsSnapshot.hub.limits && channelsSnapshot.hub.limits.max_room_name_bytes) || 64;
    roomInput.value = prefillRoom || '';
    built.body.appendChild(_channelsSheetField('Channel', roomInput));
    var keyInput = document.createElement('input');
    keyInput.type = 'password';
    keyInput.className = 'nr-input-sm';
    keyInput.placeholder = 'Only if this channel requires one';
    keyInput.autocomplete = 'off';
    keyInput.maxLength = 1024;
    built.body.appendChild(_channelsSheetField('Channel key (optional)', keyInput));
    var rememberRow = document.createElement('label');
    rememberRow.className = 'rs-dialog-checkbox-row channel-key-remember';
    var rememberKey = document.createElement('input');
    rememberKey.type = 'checkbox';
    rememberKey.checked = true;
    rememberKey.disabled = true;
    var rememberLabel = document.createElement('span');
    rememberLabel.textContent = 'Remember for reconnect';
    rememberRow.appendChild(rememberKey);
    rememberRow.appendChild(rememberLabel);
    built.body.appendChild(rememberRow);
    var note = document.createElement('p');
    note.className = 'channel-sheet-copy';
    built.body.appendChild(note);
    function updateKeyPolicy() {
        rememberKey.disabled = !keyInput.value;
        var savedRoom = _channelsDurableRoom(roomInput.value.trim().toLowerCase());
        if (!keyInput.value && savedRoom && savedRoom.has_stored_join_key) {
            note.textContent = 'A saved identity-sealed key is available. Leave this blank to use it, or enter a new key to replace it after the hub confirms membership.';
        } else {
            note.textContent = 'Keys cross the authenticated Link. Ratspeak saves only identity-sealed ciphertext, and only after the hub confirms membership.';
        }
    }
    keyInput.addEventListener('input', updateKeyPolicy);
    roomInput.addEventListener('input', updateKeyPolicy);
    updateKeyPolicy();
    var error = document.createElement('div');
    error.className = 'channel-sheet-error';
    error.setAttribute('aria-live', 'polite');
    built.body.appendChild(error);

    var cancel = document.createElement('button');
    cancel.type = 'button';
    cancel.className = 'nr-btn nr-btn-secondary';
    cancel.textContent = 'Cancel';
    cancel.addEventListener('click', function() { built.dismiss(); });
    var join = document.createElement('button');
    join.type = 'button';
    join.className = 'nr-btn nr-btn-primary';
    join.textContent = 'Join';
    join.addEventListener('click', function() {
        var room = roomInput.value.trim().toLowerCase();
        if (!room) {
            error.textContent = 'Enter a channel name.';
            roomInput.focus();
            return;
        }
        var key = keyInput.value || '';
        if (_channelsUtf8Length(key) > 1024) {
            error.textContent = 'Channel keys can be at most 1024 bytes.';
            keyInput.focus();
            return;
        }
        join.disabled = true;
        join.textContent = 'Joining\u2026';
        keyInput.value = '';
        RS.invoke('join_channel', {
            args: {
                room: room,
                key: key || null,
                remember_key: !!key && rememberKey.checked
            }
        }).then(function(result) {
            channelsActiveRoom = (result && result.room) || room;
            channelsHistorySelection = null;
            if (result && result.snapshot) channelsApplySnapshot(result.snapshot);
            built.dismiss();
            channelsSelectRoom(channelsActiveRoom);
        }).catch(function(err) {
            error.textContent = (err && err.message) || 'Could not join this channel.';
            join.disabled = false;
            join.textContent = 'Join';
        });
    });
    built.footer.appendChild(cancel);
    built.footer.appendChild(join);
    _channelsPresentSheet(built, roomInput);
}

function channelsOpenHubOptions() {
    if (!channelsSnapshot.hub || typeof _rsBuildSheet !== 'function') return;
    var hub = channelsSnapshot.hub;
    var built = _rsBuildSheet({ title: _channelsHubName(hub) }, function() {});
    var copy = document.createElement('p');
    copy.className = 'channel-sheet-copy mono';
    copy.textContent = hub.destination_hash;
    built.body.appendChild(copy);
    var details = document.createElement('p');
    details.className = 'channel-sheet-copy';
    var parts = [_channelsPhaseLabel(channelsSnapshot.phase)];
    if (hub.hops != null) parts.push(hub.hops + (hub.hops === 1 ? ' hop' : ' hops'));
    if (hub.version) parts.push('Hub ' + hub.version);
    details.textContent = parts.join(' \u00b7 ');
    built.body.appendChild(details);
    if (channelsSnapshot.hub_greeting) {
        built.body.appendChild(_channelsBuildHubGreeting(channelsSnapshot.hub_greeting, true));
    }
    var trust = document.createElement('div');
    trust.className = 'channel-sheet-trust-note';
    trust.innerHTML = '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/></svg><span>This hub is authenticated and the Link is encrypted. The hub can still read and relay everything posted to its channels.</span>';
    built.body.appendChild(trust);

    var copyButton = document.createElement('button');
    copyButton.type = 'button';
    copyButton.className = 'nr-btn nr-btn-secondary';
    copyButton.textContent = 'Copy address';
    copyButton.addEventListener('click', function() {
        RS.copyText(hub.destination_hash).then(function(ok) {
            if (typeof showToast === 'function') showToast(ok ? 'Hub address copied' : 'Could not copy', ok ? 'toast-green' : 'toast-orange', 1800);
        });
    });
    var shareButton = document.createElement('button');
    shareButton.type = 'button';
    shareButton.className = 'nr-btn nr-btn-secondary';
    shareButton.textContent = 'Share hub';
    shareButton.addEventListener('click', function() {
        built.dismiss();
        setTimeout(function() {
            channelsOpenChannelShare(hub.destination_hash, null);
        }, 220);
    });
    var disconnect = document.createElement('button');
    disconnect.type = 'button';
    disconnect.className = 'nr-btn nr-btn-danger';
    disconnect.textContent = _channelsIsConnecting() ? 'Cancel connection' : 'Disconnect';
    disconnect.addEventListener('click', function() {
        channelsPendingShareJoin = null;
        disconnect.disabled = true;
        RS.invoke('disconnect_channel_hub').then(function(snapshot) {
            built.dismiss();
            channelsActiveRoom = null;
            channelsHistorySelection = null;
            channelsApplySnapshot(snapshot);
            if (typeof showToast === 'function') showToast('Channel session ended', 'toast-green', 2200);
        }).catch(function(err) {
            disconnect.disabled = false;
            if (typeof showToast === 'function') showToast((err && err.message) || 'Could not disconnect', 'toast-red', 3200);
        });
    });
    built.footer.appendChild(shareButton);
    built.footer.appendChild(copyButton);
    built.footer.appendChild(disconnect);
    _channelsPresentSheet(built, copyButton);
}

function channelsOpenRoomOptions() {
    var room = _channelsSelectedRoomView();
    var context = _channelsHistoryContext(room);
    if (!room || !context || typeof _rsBuildSheet !== 'function') return;
    var built = _rsBuildSheet({ title: room.name }, function() {});
    var copy = document.createElement('p');
    copy.className = 'channel-sheet-copy';
    if (room.history_only) {
        copy.textContent = 'This is activity retained on this device. Notification choices apply if new activity arrives for this room again.';
    } else if (room.phase === 'joining') {
        copy.textContent = 'Canceling sends a leave request in case the hub already accepted your join. This attempt will then disappear.';
    } else if (room.phase === 'error') {
        copy.textContent = room.last_error || 'The hub did not confirm this join. Try again without reconnecting, or leave this channel.';
    } else {
        copy.textContent = 'Leaving ends live membership. Activity already stored on this device remains in Local history, and the channel stays in Recents for easy rejoining.';
    }
    built.body.appendChild(copy);
    if (room.phase === 'joined' && (room.registered != null || room.topic || room.modes)) {
        var details = document.createElement('div');
        details.className = 'channel-room-details';
        if (room.topic) {
            details.appendChild(_channelsRoomDetail('Topic', room.topic));
        }
        if (room.registered != null) {
            details.appendChild(_channelsRoomDetail('Channel', room.registered ? 'Registered on this hub' : 'Created for this session'));
        }
        var modeLabels = _channelsRoomModeLabels(room.modes);
        if (modeLabels.length) details.appendChild(_channelsRoomDetail('Access', modeLabels.join(' · ')));
        built.body.appendChild(details);
    }

    var unread = _channelsRoomUnreadState(context.hub_destination_hash, context.room_name);
    var notificationSelect = document.createElement('select');
    notificationSelect.className = 'nr-select channel-room-notification-select';
    [
        ['mentions', 'Mentions'],
        ['all', 'All activity'],
        ['mute', 'Muted']
    ].forEach(function(choice) {
        var option = document.createElement('option');
        option.value = choice[0];
        option.textContent = choice[1];
        notificationSelect.appendChild(option);
    });
    notificationSelect.value = unread.notification_level;
    var policyNote = document.createElement('p');
    policyNote.className = 'channel-room-notification-note';
    var policyNoteId = 'channel-room-notification-note-' + (++_channelsFieldSeq);
    policyNote.id = policyNoteId;
    notificationSelect.setAttribute('aria-describedby', policyNoteId);
    function renderPolicyNote() {
        if (notificationSelect.value === 'all') {
            policyNote.textContent = 'Show app attention and native alerts for new messages, actions, and notices.';
        } else if (notificationSelect.value === 'mute') {
            policyNote.textContent = 'Keep unread counts in the room list, but suppress app attention and native alerts.';
        } else {
            policyNote.textContent = 'Show app attention and native alerts only when this identity is mentioned.';
        }
    }
    renderPolicyNote();
    built.body.appendChild(_channelsSheetField('Notifications', notificationSelect));
    built.body.appendChild(policyNote);
    notificationSelect.addEventListener('change', function() {
        var previous = unread.notification_level;
        var selected = notificationSelect.value;
        renderPolicyNote();
        notificationSelect.disabled = true;
        RS.invoke('set_channel_room_notification_level', {
            args: {
                hub_destination_hash: context.hub_destination_hash,
                room: context.room_name,
                notification_level: selected
            }
        }).then(function() {
            unread.notification_level = selected;
            return channelsRefreshUnread();
        }).then(function() {
            if (typeof showToast === 'function') {
                showToast('Channel notifications updated', 'toast-green', 1800);
            }
        }).catch(function(error) {
            notificationSelect.value = previous;
            renderPolicyNote();
            if (typeof showToast === 'function') {
                showToast((error && error.message) || 'Could not update channel notifications', 'toast-red', 3200);
            }
        }).then(function() {
            notificationSelect.disabled = false;
        });
    });

    var share = document.createElement('button');
    share.type = 'button';
    share.className = 'nr-btn nr-btn-secondary';
    share.textContent = 'Share';
    share.addEventListener('click', function() {
        built.dismiss();
        setTimeout(function() {
            channelsOpenChannelShare(
                context.hub_destination_hash,
                context.room_name
            );
        }, 220);
    });

    if (room.history_only) {
        var close = document.createElement('button');
        close.type = 'button';
        close.className = 'nr-btn nr-btn-secondary';
        close.textContent = 'Close';
        close.addEventListener('click', built.dismiss);
        var activeHub = channelsSnapshot.hub && channelsSnapshot.hub.destination_hash;
        var sameHub = _channelsIsConnected() &&
            String(activeHub || '').toLowerCase() === context.hub_destination_hash;
        var open = document.createElement('button');
        open.type = 'button';
        open.className = 'nr-btn nr-btn-primary';
        open.textContent = sameHub ? 'Rejoin channel' : 'Connect to hub';
        open.addEventListener('click', function() {
            built.dismiss();
            if (sameHub) {
                channelsOpenJoinSheet(context.room_name);
            } else {
                channelsOpenConnectSheet(_channelsHubByDestination(context.hub_destination_hash));
            }
        });
        built.footer.appendChild(share);
        built.footer.appendChild(close);
        built.footer.appendChild(open);
        _channelsPresentSheet(built, notificationSelect);
        return;
    }
    if (room.phase === 'error') {
        var retry = document.createElement('button');
        retry.type = 'button';
        retry.className = 'nr-btn nr-btn-primary';
        retry.textContent = 'Try again';
        retry.addEventListener('click', function() {
            built.dismiss();
            channelsOpenJoinSheet(room.name);
        });
        built.footer.appendChild(retry);
    }
    var leave = document.createElement('button');
    leave.type = 'button';
    leave.className = room.phase === 'joined' ? 'nr-btn nr-btn-danger' : 'nr-btn nr-btn-secondary';
    leave.textContent = room.phase === 'joining' ? 'Cancel join' : (room.phase === 'parting' ? 'Leaving\u2026' : 'Leave channel');
    leave.disabled = room.phase === 'parting';
    leave.addEventListener('click', function() {
        leave.disabled = true;
        _channelsPartRoom(room.name).then(function() {
            built.dismiss();
        }).catch(function(err) {
            leave.disabled = false;
            if (typeof showToast === 'function') showToast((err && err.message) || 'Could not leave channel', 'toast-red', 3200);
        });
    });
    built.footer.insertBefore(share, built.footer.firstChild);
    built.footer.appendChild(leave);
    _channelsPresentSheet(built, notificationSelect);
}

function _channelsRoomDetail(labelText, value) {
    var row = document.createElement('div');
    row.className = 'channel-room-detail';
    var label = document.createElement('span');
    label.textContent = labelText;
    var copy = document.createElement('strong');
    copy.textContent = value;
    row.appendChild(label);
    row.appendChild(copy);
    return row;
}

function _channelsPartRoom(roomName) {
    return RS.invoke('part_channel', { args: { room: roomName } }).then(function(result) {
        if (result && result.snapshot) channelsApplySnapshot(result.snapshot);
        if (_channelsCompact() && RS.viewStack && RS.viewStack.top() && RS.viewStack.top().viewId === 'channel-detail') {
            RS.viewStack.pop();
        }
        return result;
    });
}

function channelsDisconnect() {
    channelsPendingShareJoin = null;
    return RS.invoke('disconnect_channel_hub').then(function(snapshot) {
        channelsActiveRoom = null;
        channelsHistorySelection = null;
        channelsApplySnapshot(snapshot);
        return snapshot;
    }).catch(function(error) {
        if (typeof showToast === 'function') showToast((error && error.message) || 'Could not end channel session', 'toast-red', 3500);
    });
}

function _channelsHandleComposerResult(result, originRoom) {
    result = result || {};
    if (result.snapshot) channelsApplySnapshot(result.snapshot);
    var command = result.local_command;
    var targetRoom = String(result.room || originRoom || '').trim().toLowerCase();
    if (command === 'join') {
        channelsActiveRoom = targetRoom;
        channelsHistorySelection = null;
        if (result.already_joined) {
            _channelsAddLocalRoomEvent(targetRoom, 'You\u2019re already in ' + targetRoom + '.');
            channelsSelectRoom(targetRoom);
            return Promise.resolve();
        }
        channelsSelectRoom(targetRoom);
        return Promise.resolve();
    }
    if (command === 'part') {
        if (result.already_parting) {
            _channelsAddLocalRoomEvent(targetRoom, 'Already leaving ' + targetRoom + '.');
            renderChannels();
            return Promise.resolve();
        }
        if (targetRoom === channelsActiveRoom && _channelsCompact() && RS.viewStack &&
                RS.viewStack.top() && RS.viewStack.top().viewId === 'channel-detail') {
            RS.viewStack.pop();
        }
        return Promise.resolve();
    }
    return Promise.resolve();
}

function channelsSendMessage() {
    var input = _channelsEl('channel-message-input');
    var room = channelsActiveRoom ? _channelsRoomByName(channelsActiveRoom) : null;
    if (!input || !room || room.phase !== 'joined' || _channelsSendPending) return;
    var text = input.value;
    var bodyBytes = _channelsUtf8Length(_channelsMessageBody(text));
    var limit = _channelsMessageLimit();
    if (!text.trim()) return;
    if (bodyBytes > limit) {
        if (typeof showToast === 'function') showToast('Channel message exceeds the hub limit', 'toast-red', 3000);
        return;
    }
    _channelsSendPending = true;
    _channelsUpdateComposer();
    RS.invoke('send_channel_message', {
        args: { room: room.name, text: text }
    }).then(function(result) {
        input.value = '';
        input.style.height = '';
        return _channelsHandleComposerResult(result, room.name);
    }).catch(function(error) {
        if (typeof showToast === 'function') showToast((error && error.message) || 'Could not send channel message', 'toast-red', 3500);
    }).then(function() {
        _channelsSendPending = false;
        _channelsUpdateComposer();
        input.focus();
    });
}

function _channelsBindUI() {
    document.querySelectorAll('[data-message-mode]').forEach(function(button) {
        button.addEventListener('click', function() {
            var target = button.dataset.messageMode === 'channels' ? 'channels' : 'message';
            if (typeof switchView === 'function') switchView(target);
        });
    });
    document.addEventListener('click', function(event) {
        var actionEl = event.target.closest && event.target.closest('[data-channel-action]');
        if (!actionEl) return;
        var action = actionEl.dataset.channelAction;
        if (action === 'connect') channelsOpenConnectSheet();
        else if (action === 'open-owned-hub' && typeof channelHubOpenOwnHub === 'function') channelHubOpenOwnHub();
        else if (action === 'join') channelsOpenJoinSheet();
        else if (action === 'disconnect') channelsDisconnect();
        else if (action === 'retry-room') channelsOpenJoinSheet(actionEl.dataset.room || '');
        else if (action === 'leave-room') {
            actionEl.disabled = true;
            _channelsPartRoom(actionEl.dataset.room || '').catch(function(error) {
                actionEl.disabled = false;
                if (typeof showToast === 'function') showToast((error && error.message) || 'Could not leave channel', 'toast-red', 3200);
            });
        }
    });

    var connect = _channelsEl('channels-connect-btn');
    if (connect) connect.addEventListener('click', function() {
        if (typeof channelsOpenAddSheet === 'function') channelsOpenAddSheet();
        else channelsOpenConnectSheet();
    });
    var join = _channelsEl('channels-join-btn');
    if (join) join.addEventListener('click', function() { channelsOpenJoinSheet(); });
    var hubSwitcher = _channelsEl('channel-hub-switcher-btn');
    if (hubSwitcher) hubSwitcher.addEventListener('click', channelsOpenHubSwitcher);
    var hubMenu = _channelsEl('channel-hub-menu-btn');
    if (hubMenu) hubMenu.addEventListener('click', channelsOpenHubOptions);
    var roomMenu = _channelsEl('channel-room-menu-btn');
    if (roomMenu) roomMenu.addEventListener('click', channelsOpenRoomOptions);
    var back = _channelsEl('channel-room-back-btn');
    if (back) back.addEventListener('click', function() {
        if (RS.viewStack && RS.viewStack.top() && RS.viewStack.top().viewId === 'channel-detail') RS.viewStack.pop();
    });
    var members = _channelsEl('channel-members-toggle');
    if (members) members.addEventListener('click', function() {
        var layout = _channelsEl('channels-layout');
        if (layout) layout.classList.toggle('members-open');
    });
    var membersClose = _channelsEl('channel-members-close');
    if (membersClose) membersClose.addEventListener('click', channelsCloseMemberPane);
    var membersBack = _channelsEl('channel-members-back');
    if (membersBack) membersBack.addEventListener('click', _channelsShowMemberList);
    var membersScrim = _channelsEl('channel-members-scrim');
    if (membersScrim) membersScrim.addEventListener('click', channelsCloseMemberPane);
    var membersPane = _channelsEl('channel-members-pane');
    if (membersPane) membersPane.addEventListener('keydown', function(event) {
        if (event.key === 'Escape' && _channelsSelectedMemberKey) {
            event.preventDefault();
            event.stopPropagation();
            _channelsShowMemberList();
        }
    });
    var input = _channelsEl('channel-message-input');
    if (input) {
        var useMobileTypingDefaults = _channelsUsesNativeMobileTyping();
        _channelsApplyComposerTypingPolicy(input, useMobileTypingDefaults);
        input.addEventListener('beforeinput', function(event) {
            _channelsHandleComposerBeforeInput(event, useMobileTypingDefaults);
        });
        input.addEventListener('input', function() {
            input.style.height = 'auto';
            input.style.height = Math.min(input.scrollHeight, 132) + 'px';
            _channelsUpdateComposer();
        });
        input.addEventListener('keydown', function(event) {
            if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
                event.preventDefault();
                channelsSendMessage();
            }
        });
    }
    var send = _channelsEl('channel-send-btn');
    if (send) send.addEventListener('click', channelsSendMessage);
    // Hydrate global attention even when the Channels view has not been opened;
    // the writer's startup event can precede WebView listener registration.
    channelsRefreshUnread();
    renderChannels();
}

RS.listen('channels_snapshot', function(snapshot) {
    channelsApplySnapshot(snapshot);
});

// Treat push payloads as invalidation signals. A request sequence around the
// follow-up DB read prevents an older event or command response from replacing
// newer unread state on the independent Tauri delivery paths.
RS.listen('channels_unread', function() {
    channelsRefreshUnread();
});

// Hub discovery is announce-driven. The backend query reads Reticulum's
// recent announce cache, so presenting it as an active "scan" is misleading.
// Coalesce busy announce streams and refresh only while this directory matters.
RS.listen('announce_received', function() {
    if (_channelsIsConnected() || (typeof isViewActive === 'function' && !isViewActive('channels'))) {
        _channelsLastHubRefreshAt = 0;
        return;
    }
    if (_channelsDiscoveryRefreshTimer) return;
    _channelsDiscoveryRefreshTimer = setTimeout(function() {
        _channelsDiscoveryRefreshTimer = null;
        channelsRefreshAvailableHubs();
    }, 750);
});

RS.listen('lxmf_identity', function() {
    if (typeof _channelsHubSwitcherDismiss === 'function') {
        var dismissHubSwitcher = _channelsHubSwitcherDismiss;
        _channelsHubSwitcherDismiss = null;
        dismissHubSwitcher();
    }
    channelsSavedHubs = [];
    channelsSavedRooms = [];
    channelsRoomIndex = [];
    channelsUnread = {
        rooms: [],
        unread_total: 0,
        mention_total: 0,
        attention_total: 0
    };
    _channelsUnreadRequestSeq++;
    if (typeof setMessageUnreadSource === 'function') {
        setMessageUnreadSource('channels', 0);
    }
    channelsActiveRoom = null;
    channelsHistorySelection = null;
    channelsPendingHubLabel = '';
    _channelsLocalRoomEvents = {};
    _channelsExpandedPresenceGroups = {};
    _channelsSelectedMemberKey = null;
    _channelsMemberReturnFocusKey = null;
    _channelsSavedRoomsHub = null;
    _channelsSaveHubKey = null;
    _channelsSaveHubPromise = null;
    _channelsSavedRoomKeys = {};
    _channelsHistoryCache = {};
    _channelsHistoryEpoch++;
    _channelsRenderedRoomKey = '';
    _channelsLoadedAt = 0;
    _channelsLastHubRefreshAt = 0;
    setTimeout(function() { channelsRefreshUnread(); }, 150);
    if (typeof currentView !== 'undefined' && currentView === 'channels') {
        setTimeout(function() { channelsLoad(true); }, 150);
    } else {
        renderChannels();
    }
});

document.addEventListener('visibilitychange', function() {
    if (!document.hidden) setTimeout(function() { channelsPrepareVisibleRead(); }, 0);
});

document.addEventListener('DOMContentLoaded', _channelsBindUI);
