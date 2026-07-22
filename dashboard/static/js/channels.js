// Channels: live, session-scoped group conversations. This module deliberately
// never writes transcript or membership data to browser or app storage.

var channelsSnapshot = {
    protocol_version: '0.1.3',
    phase: 'unavailable',
    nickname: null,
    hub: null,
    rooms: [],
    notices: [],
    last_error: null,
    updated_at_ms: 0
};
var channelsDiscoveredHubs = [];
var channelsSavedHubs = [];
var channelsSavedRooms = [];
var channelsActiveRoom = null;
var channelsPendingHubLabel = '';
var _channelsLoadedAt = 0;
var _channelsLastScanAt = 0;
var _channelsLoadPromise = null;
var _channelsScanPromise = null;
var _channelsSavedRoomsHub = null;
var _channelsSaveHubKey = null;
var _channelsSaveHubPromise = null;
var _channelsSavedRoomKeys = {};
var _channelsSendPending = false;
var _channelsFieldSeq = 0;

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
        channelsSnapshot.phase === 'awaiting_welcome';
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

function _channelsMessageBody(value) {
    var text = String(value || '');
    return text.indexOf('/me ') === 0 ? text.slice(4) : text;
}

function _channelsMessageLimit() {
    var limits = channelsSnapshot.hub && channelsSnapshot.hub.limits;
    return (limits && limits.max_message_body_bytes) || 350;
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
    var name = '';
    try { name = localStorage.getItem('ratspeak_identity_name') || ''; } catch (_) {}
    if (!name && typeof activeIdentity === 'function') {
        var active = activeIdentity();
        if (active) name = active.display_name || active.nickname || '';
    }
    name = String(name || 'rat').trim();
    return name.slice(0, 32) || 'rat';
}

function _channelsHubName(hub) {
    hub = hub || {};
    return hub.name || hub.announced_name || hub.label || 'Channel hub';
}

function _channelsPhaseLabel(phase) {
    switch (phase) {
        case 'resolving': return 'Finding path';
        case 'connecting': return 'Securing link';
        case 'awaiting_welcome': return 'Waiting for hub';
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
    return Object.keys(byHash).map(function(key) { return byHash[key]; }).sort(function(a, b) {
        if (a.nearby !== b.nearby) return a.nearby ? -1 : 1;
        return (b.last_seen || 0) - (a.last_seen || 0);
    });
}

function _channelsSetText(id, value) {
    var el = _channelsEl(id);
    if (el) el.textContent = value == null ? '' : String(value);
}

function channelsApplySnapshot(snapshot) {
    if (!snapshot || typeof snapshot !== 'object') return;
    var oldHub = channelsSnapshot.hub && channelsSnapshot.hub.destination_hash;
    channelsSnapshot = snapshot;
    if (!Array.isArray(channelsSnapshot.rooms)) channelsSnapshot.rooms = [];
    if (!Array.isArray(channelsSnapshot.notices)) channelsSnapshot.notices = [];

    var newHub = channelsSnapshot.hub && channelsSnapshot.hub.destination_hash;
    if (newHub && newHub !== oldHub) {
        channelsSavedRooms = [];
        _channelsSavedRoomsHub = null;
        _channelsSavedRoomKeys = {};
        channelsLoadSavedRooms(newHub);
    }

    if (channelsActiveRoom && !_channelsRoomByName(channelsActiveRoom)) {
        channelsActiveRoom = null;
    }
    if (!channelsActiveRoom && channelsSnapshot.rooms.length) {
        channelsActiveRoom = channelsSnapshot.rooms[0].name;
    }

    _channelsPersistConveniences();
    renderChannels();
}

function channelsLoad(force) {
    var now = Date.now();
    if (!force && _channelsLoadPromise) return _channelsLoadPromise;
    if (!force && _channelsLoadedAt && now - _channelsLoadedAt < 1500) {
        renderChannels();
        return Promise.resolve(channelsSnapshot);
    }

    _channelsLoadPromise = Promise.all([
        RS.invoke('api_channels'),
        RS.invoke('api_saved_channel_hubs').catch(function() { return []; })
    ]).then(function(results) {
        _channelsLoadedAt = Date.now();
        channelsSavedHubs = Array.isArray(results[1]) ? results[1] : [];
        channelsApplySnapshot(results[0]);
        if (channelsSnapshot.hub && channelsSnapshot.hub.destination_hash) {
            channelsLoadSavedRooms(channelsSnapshot.hub.destination_hash);
        }
        if (!_channelsIsConnected() && Date.now() - _channelsLastScanAt > 15000) {
            channelsScan(false);
        }
        return channelsSnapshot;
    }).catch(function(error) {
        channelsSnapshot.phase = 'unavailable';
        channelsSnapshot.last_error = error && error.message ? error.message : 'Channels are unavailable';
        renderChannels();
        return channelsSnapshot;
    }).then(function(result) {
        _channelsLoadPromise = null;
        return result;
    });
    return _channelsLoadPromise;
}

function channelsScan(explicit) {
    if (_channelsScanPromise) return _channelsScanPromise;
    var button = _channelsEl('channels-refresh-btn');
    if (button) {
        button.disabled = true;
        button.textContent = 'Scanning';
    }
    _channelsScanPromise = RS.invoke('discover_channel_hubs').then(function(hubs) {
        channelsDiscoveredHubs = Array.isArray(hubs) ? hubs : [];
        _channelsLastScanAt = Date.now();
        renderChannels();
        if (explicit && channelsDiscoveredHubs.length === 0 && typeof showToast === 'function') {
            showToast('No channel hubs heard yet', 'toast-orange', 2600);
        }
        return channelsDiscoveredHubs;
    }).catch(function(error) {
        if (explicit && typeof showToast === 'function') {
            showToast((error && error.message) || 'Could not scan for channel hubs', 'toast-red', 3500);
        }
        return [];
    }).then(function(result) {
        _channelsScanPromise = null;
        var refresh = _channelsEl('channels-refresh-btn');
        if (refresh) {
            refresh.disabled = false;
            refresh.textContent = 'Scan';
        }
        return result;
    });
    return _channelsScanPromise;
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
        renderChannels();
        return channelsSavedRooms;
    }).catch(function() {
        if (_channelsSavedRoomsHub === destination) channelsSavedRooms = [];
        return [];
    });
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
    var menu = _channelsEl('channel-hub-menu-btn');
    var subtitle = _channelsEl('channels-sidebar-subtitle');
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
    if (subtitle) subtitle.textContent = 'Group conversations';
}

function _channelsRenderList() {
    var list = _channelsEl('channels-list');
    var label = _channelsEl('channels-list-label');
    var join = _channelsEl('channels-join-btn');
    var scan = _channelsEl('channels-refresh-btn');
    if (!list) return;
    list.textContent = '';

    if (_channelsIsConnected()) {
        if (label) label.textContent = 'Channels';
        if (join) join.hidden = false;
        if (scan) scan.hidden = true;
        var liveNames = {};
        channelsSnapshot.rooms.forEach(function(room) {
            liveNames[room.name] = true;
            list.appendChild(_channelsBuildRoomRow(room, false));
        });
        channelsSavedRooms.forEach(function(saved) {
            if (!liveNames[saved.room_name]) list.appendChild(_channelsBuildRoomRow({ name: saved.room_name }, true));
        });
        if (!list.children.length) {
            list.appendChild(_channelsEmptyList('No channels joined', 'Join a channel', 'join'));
        }
        return;
    }

    if (label) label.textContent = 'Nearby & recent';
    if (join) join.hidden = true;
    if (scan) scan.hidden = false;
    var hubs = _channelsMergedHubs();
    hubs.forEach(function(hub) { list.appendChild(_channelsBuildHubRow(hub)); });
    if (!hubs.length) {
        var emptyText = _channelsIsConnecting() ? 'Connecting to hub\u2026' : 'No channel hubs yet';
        list.appendChild(_channelsEmptyList(emptyText, 'Connect to a hub', 'connect'));
    }
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

function _channelsBuildHubRow(hub) {
    var row = document.createElement('button');
    row.type = 'button';
    row.className = 'channel-hub-row';
    row.dataset.destinationHash = hub.destination_hash;

    var icon = document.createElement('span');
    icon.className = 'channel-hub-row-icon';
    icon.innerHTML = _channelsRadioIcon();
    row.appendChild(icon);

    var copy = document.createElement('span');
    copy.className = 'channel-hub-row-copy';
    var title = document.createElement('span');
    title.className = 'channel-hub-row-title';
    title.textContent = _channelsHubName(hub);
    var meta = document.createElement('span');
    meta.className = 'channel-hub-row-meta';
    meta.textContent = (hub.nearby && hub.hops != null ? hub.hops + (hub.hops === 1 ? ' hop' : ' hops') + ' \u00b7 ' : '') + _channelsShortHash(hub.destination_hash);
    copy.appendChild(title);
    copy.appendChild(meta);
    row.appendChild(copy);

    var status = document.createElement('span');
    status.className = 'channel-row-status';
    status.textContent = hub.nearby ? 'Nearby' : 'Recent';
    row.appendChild(status);
    row.addEventListener('click', function() { channelsOpenConnectSheet(hub); });
    return row;
}

function _channelsBuildRoomRow(room, savedOnly) {
    var row = document.createElement('button');
    row.type = 'button';
    row.className = 'channel-room-row' + (!savedOnly && room.name === channelsActiveRoom ? ' active' : '');
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
        meta.textContent = 'Joined before \u00b7 tap to rejoin';
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

    if (savedOnly || room.phase !== 'joined') {
        var status = document.createElement('span');
        status.className = 'channel-row-status' + (!savedOnly && room.phase === 'error' ? ' error' : '');
        status.textContent = savedOnly ? 'Recent' : _channelsRoomPhaseLabel(room.phase);
        row.appendChild(status);
    }

    row.addEventListener('click', function() {
        if (savedOnly) channelsOpenJoinSheet(room.name);
        else channelsSelectRoom(room.name);
    });
    return row;
}

function _channelsRenderRoom() {
    var layout = _channelsEl('channels-layout');
    var header = _channelsEl('channel-room-header');
    var compose = _channelsEl('channel-compose');
    var transcript = _channelsEl('channel-transcript');
    var room = channelsActiveRoom ? _channelsRoomByName(channelsActiveRoom) : null;
    if (!header || !transcript || !compose) return;
    if (layout) layout.classList.toggle('has-active-room', !!room);
    if (layout) layout.classList.toggle('room-live', !!room && room.phase === 'joined');

    if (!room) {
        if (layout) layout.classList.remove('members-open');
        header.hidden = true;
        compose.hidden = true;
        _channelsRenderRoomEmpty(transcript);
        _channelsRenderMembers(null);
        return;
    }

    header.hidden = false;
    compose.hidden = room.phase !== 'joined';
    if (room.phase !== 'joined' && layout) layout.classList.remove('members-open');
    var membersToggle = _channelsEl('channel-members-toggle');
    if (membersToggle) membersToggle.hidden = room.phase !== 'joined';
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
    } else {
        roomMeta = memberCount ? memberCount + (memberCount === 1 ? ' person here' : ' people here') : 'No member list';
        if (room.topic) roomMeta = room.topic + (memberCount ? ' \u00b7 ' + roomMeta : '');
    }
    _channelsSetText('channel-room-meta', roomMeta);

    var wasNearBottom = transcript.scrollHeight - transcript.scrollTop - transcript.clientHeight < 90;
    transcript.textContent = '';
    var items = [];
    channelsSnapshot.notices.forEach(function(item) { items.push({ item: item, hubNotice: true }); });
    (room.transcript || []).forEach(function(item) {
        items.push({ item: item, hubNotice: _channelsIsHubNotice(item) });
    });
    items.sort(function(a, b) { return (a.item.timestamp_ms || 0) - (b.item.timestamp_ms || 0); });
    if (room.phase !== 'joined') {
        transcript.appendChild(_channelsBuildRoomTransition(room));
    }
    if (!items.length && room.phase === 'joined') {
        var waiting = document.createElement('div');
        waiting.className = 'channel-welcome-state';
        var waitingTitle = document.createElement('h3');
        waitingTitle.textContent = 'Ready when you are';
        var waitingCopy = document.createElement('p');
        waitingCopy.textContent = 'Messages will appear here as people post.';
        waiting.appendChild(waitingTitle);
        waiting.appendChild(waitingCopy);
        transcript.appendChild(waiting);
    } else if (items.length) {
        items.forEach(function(entry) {
            transcript.appendChild(_channelsBuildTranscriptItem(entry.item, entry.hubNotice));
        });
    }
    if (wasNearBottom || channelsActiveRoom !== room.name) {
        requestAnimationFrame(function() { transcript.scrollTop = transcript.scrollHeight; });
    }
    _channelsRenderMembers(room);
    _channelsUpdateComposer();
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
    } else if (channelsSnapshot.phase === 'error') {
        title.textContent = 'The channel session ended';
        copy.textContent = channelsSnapshot.last_error || 'The channel Link closed. Reconnect when you are ready.';
        button.dataset.channelAction = 'connect';
        button.textContent = 'Reconnect';
    } else {
        title.textContent = 'Join a conversation';
        copy.textContent = 'Connect to a trusted hub, then choose a channel.';
        button.dataset.channelAction = 'connect';
        button.textContent = 'Find a hub';
    }
    state.appendChild(mark);
    state.appendChild(title);
    state.appendChild(copy);
    state.appendChild(button);
    transcript.appendChild(state);
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
    var body = document.createElement('div');
    body.className = 'channel-hub-notice-text';
    body.textContent = item.text || '';
    heading.appendChild(label);
    heading.appendChild(time);
    notice.appendChild(heading);
    notice.appendChild(body);
    return notice;
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
    event.className = 'channel-event ' + kind + (item.ours ? ' ours' : '');
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
    var time = document.createElement('time');
    time.className = 'channel-event-time';
    time.dateTime = new Date(Number(item.timestamp_ms) || Date.now()).toISOString();
    time.textContent = _channelsFormatTime(item.timestamp_ms);
    var body = document.createElement('div');
    body.className = 'channel-event-text';
    body.textContent = kind === 'action' ? authorText + ' ' + (item.text || '') : (item.text || '');

    event.appendChild(author);
    event.appendChild(time);
    event.appendChild(body);
    return event;
}

function _channelsRenderMembers(room) {
    var list = _channelsEl('channel-members-list');
    var note = _channelsEl('channel-members-note');
    if (!list) return;
    list.textContent = '';
    var members = room && Array.isArray(room.members) ? room.members : [];
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
        var row = document.createElement('div');
        row.className = 'channel-member-row';
        row.dataset.tone = member.is_self ? 'self' : _channelsIdentityTone(member.identity_hash || member.nickname);
        var marker = document.createElement('span');
        marker.className = 'channel-identity-marker';
        marker.setAttribute('aria-hidden', 'true');
        var copy = document.createElement('span');
        copy.className = 'channel-member-copy';
        var name = document.createElement('span');
        name.className = 'channel-member-name';
        name.textContent = member.nickname || _channelsShortHash(member.identity_hash) || 'Channel member';
        copy.appendChild(name);
        if (member.is_self) {
            var you = document.createElement('span');
            you.className = 'channel-member-you';
            you.textContent = 'You';
            copy.appendChild(you);
        }
        row.appendChild(marker);
        row.appendChild(copy);
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

function channelsSelectRoom(roomName) {
    var room = _channelsRoomByName(roomName);
    if (!room) return;
    channelsActiveRoom = room.name;
    renderChannels();
    if (_channelsCompact() && RS.viewStack) {
        var top = RS.viewStack.top();
        if (!top || top.viewId !== 'channel-detail') {
            RS.viewStack.push('channel-detail', { room: room.name });
        }
    }
}

function _onChannelDetailExit() {
    channelsCloseMemberPane();
    var input = _channelsEl('channel-message-input');
    if (input) input.blur();
}

function channelsCloseMemberPane() {
    var layout = _channelsEl('channels-layout');
    if (layout) layout.classList.remove('members-open');
}

function channelsOpenConnectSheet(prefill) {
    if (typeof _rsBuildSheet !== 'function') return;
    prefill = prefill || {};
    var hubs = _channelsMergedHubs();
    var selectedHash = prefill.destination_hash || (channelsSnapshot.hub && channelsSnapshot.hub.destination_hash) || '';
    var selectedLabel = prefill.label || prefill.announced_name || '';
    var defaultNick = prefill.nickname || (_channelsSavedHub(selectedHash) || {}).nickname || channelsSnapshot.nickname || _channelsDefaultNickname();
    var built = _rsBuildSheet({ title: 'Connect to Channels' }, function() {});

    var copy = document.createElement('p');
    copy.className = 'channel-sheet-copy';
    copy.textContent = 'Choose a recently heard hub or enter its destination. Ratspeak will authenticate the connection.';
    built.body.appendChild(copy);

    var trust = document.createElement('div');
    trust.className = 'channel-sheet-trust-note';
    trust.innerHTML = '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg><span>Choose a hub you trust. The Link is encrypted in transit, but the hub relays and can read channel messages.</span>';
    built.body.appendChild(trust);

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
        if (destinationInput.value.trim().toLowerCase() !== selectedHash) selectedLabel = '';
    });

    if (hubs.length) {
        var availableLabel = document.createElement('div');
        availableLabel.className = 'channels-section-label';
        availableLabel.textContent = 'Nearby & recent';
        built.body.appendChild(availableLabel);
        var available = document.createElement('div');
        available.className = 'channel-sheet-hubs';
        hubs.forEach(function(hub) {
            var row = document.createElement('button');
            row.type = 'button';
            row.className = 'channel-sheet-hub' + (hub.destination_hash === selectedHash ? ' selected' : '');
            var icon = document.createElement('span');
            icon.className = 'channel-hub-row-icon';
            icon.innerHTML = _channelsRadioIcon();
            var rowCopy = document.createElement('span');
            rowCopy.className = 'channel-sheet-hub-copy';
            var title = document.createElement('strong');
            title.textContent = _channelsHubName(hub);
            var hash = document.createElement('span');
            hash.textContent = _channelsShortHash(hub.destination_hash);
            rowCopy.appendChild(title);
            rowCopy.appendChild(hash);
            row.appendChild(icon);
            row.appendChild(rowCopy);
            row.addEventListener('click', function() {
                selectedHash = hub.destination_hash;
                selectedLabel = hub.label || hub.announced_name || '';
                destinationInput.value = selectedHash;
                if (hub.nickname) nicknameInput.value = hub.nickname;
                available.querySelectorAll('.channel-sheet-hub').forEach(function(el) { el.classList.remove('selected'); });
                row.classList.add('selected');
                error.textContent = '';
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
    connect.textContent = 'Connect';
    connect.addEventListener('click', function() {
        var destination = destinationInput.value.trim().toLowerCase();
        var nickname = nicknameInput.value.trim();
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
        connect.disabled = true;
        connect.textContent = 'Connecting\u2026';
        error.textContent = '';
        channelsPendingHubLabel = selectedLabel;
        RS.invoke('connect_channel_hub', {
            args: { destination_hash: destination, nickname: nickname }
        }).then(function(snapshot) {
            channelsActiveRoom = null;
            channelsApplySnapshot(snapshot);
            built.dismiss();
            if (typeof showToast === 'function') showToast('Connecting to channel hub\u2026', 'toast-blue', 2600);
        }).catch(function(err) {
            error.textContent = (err && err.message) || 'Could not connect to this hub.';
            connect.disabled = false;
            connect.textContent = 'Connect';
        });
    });
    built.footer.appendChild(cancel);
    built.footer.appendChild(connect);
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
    roomInput.maxLength = (channelsSnapshot.hub && channelsSnapshot.hub.limits && channelsSnapshot.hub.limits.max_room_name_bytes) || 64;
    roomInput.value = prefillRoom || '';
    built.body.appendChild(_channelsSheetField('Channel', roomInput));
    var keyInput = document.createElement('input');
    keyInput.type = 'password';
    keyInput.className = 'nr-input-sm';
    keyInput.placeholder = 'Only if this channel requires one';
    keyInput.autocomplete = 'off';
    built.body.appendChild(_channelsSheetField('Channel key (optional)', keyInput));
    var note = document.createElement('p');
    note.className = 'channel-sheet-copy';
    note.textContent = 'Keys are sent over the authenticated Link and are never saved by Ratspeak.';
    built.body.appendChild(note);
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
        join.disabled = true;
        join.textContent = 'Joining\u2026';
        RS.invoke('join_channel', {
            args: { room: room, key: keyInput.value || null }
        }).then(function(result) {
            channelsActiveRoom = (result && result.room) || room;
            built.dismiss();
            channelsLoad(true).then(function() { channelsSelectRoom(channelsActiveRoom); });
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
    var disconnect = document.createElement('button');
    disconnect.type = 'button';
    disconnect.className = 'nr-btn nr-btn-danger';
    disconnect.textContent = _channelsIsConnecting() ? 'Cancel connection' : 'Disconnect';
    disconnect.addEventListener('click', function() {
        disconnect.disabled = true;
        RS.invoke('disconnect_channel_hub').then(function(snapshot) {
            built.dismiss();
            channelsActiveRoom = null;
            channelsApplySnapshot(snapshot);
            if (typeof showToast === 'function') showToast('Channel session ended', 'toast-green', 2200);
        }).catch(function(err) {
            disconnect.disabled = false;
            if (typeof showToast === 'function') showToast((err && err.message) || 'Could not disconnect', 'toast-red', 3200);
        });
    });
    built.footer.appendChild(copyButton);
    built.footer.appendChild(disconnect);
    _channelsPresentSheet(built, copyButton);
}

function channelsOpenRoomOptions() {
    var room = channelsActiveRoom ? _channelsRoomByName(channelsActiveRoom) : null;
    if (!room || typeof _rsBuildSheet !== 'function') return;
    var built = _rsBuildSheet({ title: room.name }, function() {});
    var copy = document.createElement('p');
    copy.className = 'channel-sheet-copy';
    if (room.phase === 'joining') {
        copy.textContent = 'Canceling sends a leave request in case the hub already accepted your join. This attempt will then disappear.';
    } else if (room.phase === 'error') {
        copy.textContent = room.last_error || 'The hub did not confirm this join. Try again without reconnecting, or leave this channel.';
    } else {
        copy.textContent = 'Leaving removes you from this channel and clears its current transcript. The channel remains in Recents for easy rejoining.';
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
    built.footer.appendChild(leave);
    _channelsPresentSheet(built, leave);
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
        if (_channelsCompact() && RS.viewStack && RS.viewStack.top() && RS.viewStack.top().viewId === 'channel-detail') {
            RS.viewStack.pop();
        }
        return result;
    });
}

function channelsDisconnect() {
    return RS.invoke('disconnect_channel_hub').then(function(snapshot) {
        channelsActiveRoom = null;
        channelsApplySnapshot(snapshot);
        return snapshot;
    }).catch(function(error) {
        if (typeof showToast === 'function') showToast((error && error.message) || 'Could not end channel session', 'toast-red', 3500);
    });
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
    }).then(function() {
        input.value = '';
        input.style.height = '';
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
    if (connect) connect.addEventListener('click', function() { channelsOpenConnectSheet(); });
    var refresh = _channelsEl('channels-refresh-btn');
    if (refresh) refresh.addEventListener('click', function() { channelsScan(true); });
    var join = _channelsEl('channels-join-btn');
    if (join) join.addEventListener('click', function() { channelsOpenJoinSheet(); });
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
    var membersScrim = _channelsEl('channel-members-scrim');
    if (membersScrim) membersScrim.addEventListener('click', channelsCloseMemberPane);
    var input = _channelsEl('channel-message-input');
    if (input) {
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
    renderChannels();
}

RS.listen('channels_snapshot', function(snapshot) {
    channelsApplySnapshot(snapshot);
});

RS.listen('lxmf_identity', function() {
    channelsSavedHubs = [];
    channelsSavedRooms = [];
    channelsActiveRoom = null;
    channelsPendingHubLabel = '';
    _channelsSavedRoomsHub = null;
    _channelsSaveHubKey = null;
    _channelsSaveHubPromise = null;
    _channelsSavedRoomKeys = {};
    _channelsLoadedAt = 0;
    if (typeof currentView !== 'undefined' && currentView === 'channels') {
        setTimeout(function() { channelsLoad(true); }, 150);
    } else {
        renderChannels();
    }
});

document.addEventListener('DOMContentLoaded', _channelsBindUI);
