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
var _channelsLiveItemSeenAt = {};
var _channelsSelectedMemberKey = null;
var _channelsMemberReturnFocusKey = null;
var _channelsHistoryCache = {};
var _channelsHistoryRequestSeq = 0;
var _channelsParticipantRequestSeq = 0;
var _channelsHistoryEpoch = 0;
var _channelsRoomIndexRequestSeq = 0;
var _channelsUnreadRequestSeq = 0;
var _channelsRenderedRoomKey = '';
var _channelsHubSwitcherDismiss = null;
var _channelsMemberDetailDismiss = null;
var _channelsHubPulseTimer = null;
var _channelsObservedMembersByRoom = {};
var _channelsMemberContinuityTimer = null;
var _channelsPublicConsent = {
    loaded: false,
    acceptedVersion: 0,
    requiredVersion: 1
};
var _channelsPublicConsentLoadPromise = null;
var _channelsPublicConsentPromptPromise = null;
var _channelsBlockedAddresses = {};
var _channelsBlockedLoadPromise = null;
var CHANNEL_HUB_PULSE_INTERVAL_MS = 60 * 1000;
var CHANNEL_MEMBER_CONTINUITY_MS = 60 * 1000;
var CHANNEL_MESSAGE_GROUP_WINDOW_MS = 5 * 60 * 1000;
var CHANNEL_HISTORY_PAGE_SIZE = 100;
var CHANNEL_HISTORY_SYNC_PAGE_SIZE = 200;
var CHANNEL_HISTORY_CACHE_ROOM_LIMIT = 5000;
var CHANNEL_HISTORY_MAX_SYNC_PAGES = 32;
var CHANNEL_DIRECTORY_STALE_AFTER_MS = 5 * 60 * 1000;

function _channelsEl(id) {
    return document.getElementById(id);
}

function _channelsCompact() {
    return typeof isCompactLayout === 'function' && isCompactLayout();
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

function _channelsApplyPublicConsentSettings(data) {
    if (!data) return;
    if (data.public_channel_consent_required_version !== undefined) {
        _channelsPublicConsent.requiredVersion = Math.max(
            1,
            parseInt(data.public_channel_consent_required_version, 10) || 1
        );
    }
    if (data.public_channel_consent_version !== undefined) {
        _channelsPublicConsent.acceptedVersion = Math.max(
            0,
            parseInt(data.public_channel_consent_version, 10) || 0
        );
    }
    _channelsPublicConsent.loaded = true;
}

function _channelsHasPublicConsent() {
    return _channelsPublicConsent.loaded &&
        _channelsPublicConsent.acceptedVersion === _channelsPublicConsent.requiredVersion;
}

function _channelsLoadPublicConsent() {
    if (_channelsPublicConsent.loaded) return Promise.resolve(_channelsPublicConsent);
    if (_channelsPublicConsentLoadPromise) return _channelsPublicConsentLoadPromise;
    _channelsPublicConsentLoadPromise = RS.invoke('api_app_settings').then(function(data) {
        _channelsApplyPublicConsentSettings(data);
        return _channelsPublicConsent;
    }).finally(function() {
        _channelsPublicConsentLoadPromise = null;
    });
    return _channelsPublicConsentLoadPromise;
}

function _channelsPublicConsentLink(label, documentId) {
    var button = document.createElement('button');
    button.type = 'button';
    button.className = 'channel-consent-policy-link';
    button.textContent = label;
    button.addEventListener('click', function(event) {
        event.preventDefault();
        event.stopPropagation();
        if (!RS.legal || typeof RS.legal.open !== 'function' || !RS.legal.open(documentId)) {
            if (typeof showToast === 'function') {
                showToast('This document could not be opened.', 'toast-error', 5000);
            }
        }
    });
    return button;
}

function _channelsShowPublicConsent() {
    if (_channelsHasPublicConsent()) return Promise.resolve(true);
    if (_channelsPublicConsentPromptPromise) return _channelsPublicConsentPromptPromise;
    if (typeof _rsBuildSheet !== 'function') return Promise.resolve(false);

    _channelsPublicConsentPromptPromise = new Promise(function(resolve) {
        var built = _rsBuildSheet({ title: 'Public channels' }, function(value) {
            _channelsPublicConsentPromptPromise = null;
            resolve(value === true);
        });
        built.sheet.classList.add('channel-consent-sheet');

        var intro = document.createElement('p');
        intro.className = 'channel-consent-intro';
        intro.textContent = 'Public channels are shared spaces hosted by Ratspeak or independent operators.';
        built.body.appendChild(intro);

        var facts = document.createElement('div');
        facts.className = 'channel-consent-facts';
        [
            ['hub', 'Independent operators', 'Unless marked official, a hub is run and moderated by someone else. Ratspeak may not be able to remove its content.'],
            ['privacy', 'Different privacy', 'A hub can read and relay channel messages. Direct-message encryption does not apply.'],
            ['controls', 'Your controls', 'You can block participants, report content, and leave a hub at any time.']
        ].forEach(function(fact) {
            var item = document.createElement('div');
            item.className = 'channel-consent-fact';
            var icon = document.createElement('span');
            icon.className = 'channel-consent-fact-icon';
            icon.setAttribute('aria-hidden', 'true');
            icon.dataset.icon = fact[0];
            if (fact[0] === 'hub') {
                icon.innerHTML = '<svg viewBox="0 0 24 24"><path d="M4 20h16M6 20V9l6-5 6 5v11M9 13h6M9 16h6"/></svg>';
            } else if (fact[0] === 'privacy') {
                icon.innerHTML = '<svg viewBox="0 0 24 24"><path d="M12 3 4.5 6v5.5c0 4.5 3 7.8 7.5 9.5 4.5-1.7 7.5-5 7.5-9.5V6L12 3Z"/><path d="M9 12h6"/></svg>';
            } else {
                icon.innerHTML = '<svg viewBox="0 0 24 24"><path d="M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18Z"/><path d="m8.5 12 2.2 2.2 4.8-5"/></svg>';
            }
            var text = document.createElement('span');
            text.className = 'channel-consent-fact-copy';
            var title = document.createElement('strong');
            title.textContent = fact[1];
            var copy = document.createElement('span');
            copy.textContent = fact[2];
            text.appendChild(title);
            text.appendChild(copy);
            item.appendChild(icon);
            item.appendChild(text);
            facts.appendChild(item);
        });
        built.body.appendChild(facts);

        var acknowledgements = document.createElement('div');
        acknowledgements.className = 'channel-consent-acknowledgements';
        function acknowledgement(labelText) {
            var label = document.createElement('label');
            label.className = 'channel-consent-check';
            var input = document.createElement('input');
            input.type = 'checkbox';
            var copy = document.createElement('span');
            copy.textContent = labelText;
            label.appendChild(input);
            label.appendChild(copy);
            acknowledgements.appendChild(label);
            return input;
        }
        var adult = acknowledgement('I am 18 or older.');
        var independent = acknowledgement('I understand that public channels may contain content from people Ratspeak does not control.');
        built.body.appendChild(acknowledgements);

        var agreement = document.createElement('p');
        agreement.className = 'channel-consent-agreement';
        agreement.textContent = 'By continuing, you agree to the Terms and Community Guidelines.';
        built.body.appendChild(agreement);

        var policies = document.createElement('div');
        policies.className = 'channel-consent-policies';
        [
            ['Privacy', 'privacy'],
            ['Terms', 'terms'],
            ['Guidelines', 'guidelines'],
            ['Support', 'support']
        ].forEach(function(policy, index) {
            if (index > 0) {
                var separator = document.createElement('span');
                separator.className = 'channel-consent-policy-separator';
                separator.setAttribute('aria-hidden', 'true');
                separator.textContent = '·';
                policies.appendChild(separator);
            }
            policies.appendChild(_channelsPublicConsentLink(policy[0], policy[1]));
        });
        built.body.appendChild(policies);

        var error = document.createElement('div');
        error.className = 'channel-sheet-error';
        error.setAttribute('aria-live', 'polite');
        built.body.appendChild(error);

        var cancel = document.createElement('button');
        cancel.type = 'button';
        cancel.className = 'nr-btn nr-btn-secondary';
        cancel.textContent = 'Not now';
        cancel.addEventListener('click', function() { built.dismiss(false); });
        var continueButton = document.createElement('button');
        continueButton.type = 'button';
        continueButton.className = 'nr-btn nr-btn-primary';
        continueButton.textContent = 'Continue';
        continueButton.disabled = true;
        function syncContinue() {
            continueButton.disabled = !(adult.checked && independent.checked);
            error.textContent = '';
        }
        adult.addEventListener('change', syncContinue);
        independent.addEventListener('change', syncContinue);
        continueButton.addEventListener('click', function() {
            if (!adult.checked || !independent.checked) return;
            continueButton.disabled = true;
            continueButton.textContent = 'Saving\u2026';
            RS.invoke('accept_public_channel_consent', {
                version: _channelsPublicConsent.requiredVersion,
                adultConfirmed: adult.checked,
                independentHubsUnderstood: independent.checked,
                policiesAccepted: true
            }).then(function(data) {
                _channelsApplyPublicConsentSettings(data);
                built.dismiss(true);
            }).catch(function(err) {
                error.textContent = (err && err.message) || 'Could not save this acknowledgement.';
                continueButton.textContent = 'Continue';
                syncContinue();
            });
        });
        built.footer.appendChild(cancel);
        built.footer.appendChild(continueButton);
        _channelsPresentSheet(built, adult);
    });
    return _channelsPublicConsentPromptPromise;
}

function _channelsEnsurePublicConsent() {
    return _channelsLoadPublicConsent().then(function() {
        return _channelsHasPublicConsent() ? true : _channelsShowPublicConsent();
    });
}

function _channelsApplyBlockedContacts(rows) {
    var blocked = {};
    (Array.isArray(rows) ? rows : []).forEach(function(row) {
        var hash = String(row && (row.hash || row.dest_hash) || '').trim().toLowerCase();
        if (/^[0-9a-f]{32}$/.test(hash)) blocked[hash] = true;
    });
    _channelsBlockedAddresses = blocked;
}

function _channelsLoadBlockedContacts() {
    if (_channelsBlockedLoadPromise) return _channelsBlockedLoadPromise;
    _channelsBlockedLoadPromise = RS.invoke('api_blocked_contacts').then(function(rows) {
        _channelsApplyBlockedContacts(rows);
        return _channelsBlockedAddresses;
    }).finally(function() {
        _channelsBlockedLoadPromise = null;
    });
    return _channelsBlockedLoadPromise;
}

function _channelsIsBlockedAddress(value) {
    return !!_channelsBlockedAddresses[String(value || '').trim().toLowerCase()];
}

function _channelsIsBlockedItem(item) {
    if (!item || item.ours) return false;
    var canonical = String(item.source_lxmf_hash || '').trim().toLowerCase();
    if (!canonical && item.source_hash) {
        canonical = _channelsIdentityAvatarSeed(item.source_hash, '', false);
    }
    return _channelsIsBlockedAddress(canonical);
}

function _channelsIsBlockedMember(member) {
    if (!member || member.is_self) return false;
    var details = _channelsMemberDetails(member);
    return _channelsIsBlockedAddress(details.lxmfAddress);
}

function _channelsRenderAfterSafetyChange() {
    var room = channelsActiveRoom ? _channelsRoomByName(channelsActiveRoom) : null;
    if (room) _channelsRenderRoom();
    else renderChannels();
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
    if (typeof RS !== 'undefined' && RS.text && typeof RS.text.utf8Length === 'function') {
        return RS.text.utf8Length(value);
    }
    var text = String(value || '');
    if (window.TextEncoder) return new TextEncoder().encode(text).length;
    return unescape(encodeURIComponent(text)).length;
}

function _channelsUtf8Truncate(value, maxBytes) {
    if (typeof RS !== 'undefined' && RS.text && typeof RS.text.truncateUtf8 === 'function') {
        return RS.text.truncateUtf8(value, maxBytes);
    }
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
            showToast('The channel message is already at its byte limit', 'toast-warning', 2400);
        }
        return false;
    }
    input.value = before + fitted + after;
    var cursor = before.length + fitted.length;
    if (typeof input.setSelectionRange === 'function') {
        input.setSelectionRange(cursor, cursor);
    }
    if (typeof RS !== 'undefined' && RS.composer && typeof RS.composer.resize === 'function') {
        RS.composer.resize(input);
    } else {
        input.style.height = 'auto';
        input.style.height = Math.min(input.scrollHeight, 124) + 'px';
    }
    _channelsUpdateComposer();
    if (typeof RS !== 'undefined' && RS.composer) RS.composer.focusWithoutScroll(input);
    else input.focus();
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

function _channelsDisplayDate(timestampMs) {
    var value = Number(timestampMs);
    var date = new Date(value || Date.now());
    return Number.isNaN(date.getTime()) ? new Date() : date;
}

function _channelsFormatTime(timestampMs) {
    var date = _channelsDisplayDate(timestampMs);
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

function _channelsRoomDisplayName(roomName) {
    var name = String(roomName || '');
    return name.charAt(0) === '#' ? name.slice(1) : name;
}

function _channelsRoomIndexEntry(hubDestinationHash, roomName) {
    var hub = String(hubDestinationHash || '').toLowerCase();
    var room = String(roomName || '').toLowerCase();
    if (!hub || !room) return null;
    return channelsRoomIndex.find(function(entry) {
        return String(entry.hub_destination_hash || '').toLowerCase() === hub &&
            String(entry.room_name || '').toLowerCase() === room;
    }) || null;
}

function _channelsRememberedRoomTopic(hubDestinationHash, roomName, fallback) {
    var topic = String(fallback || '').trim();
    if (topic) return topic;

    var indexed = _channelsRoomIndexEntry(hubDestinationHash, roomName);
    topic = String(indexed && indexed.topic || '').trim();
    if (topic) return topic;

    var activeHub = channelsSnapshot.hub && channelsSnapshot.hub.destination_hash;
    if (String(activeHub || '').toLowerCase() !==
            String(hubDestinationHash || '').toLowerCase()) return '';
    var target = String(roomName || '').toLowerCase();
    var liveRoom = channelsSnapshot.rooms.find(function(room) {
        return String(room.name || '').toLowerCase() === target;
    });
    topic = String(liveRoom && liveRoom.topic || '').trim();
    if (topic) return topic;
    var directoryRooms = channelsSnapshot.directory && channelsSnapshot.directory.rooms;
    if (!Array.isArray(directoryRooms)) return '';
    var directoryRoom = directoryRooms.find(function(room) {
        return String(room.name || '').toLowerCase() === target;
    });
    return String(directoryRoom && directoryRoom.topic || '').trim();
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
    if (!options.public_consent_checked && !_channelsHasPublicConsent()) {
        return _channelsEnsurePublicConsent().then(function(accepted) {
            if (!accepted) return null;
            return channelsConnectToHub(hub, Object.assign({}, options, {
                public_consent_checked: true
            }));
        });
    }
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
            topic: _channelsRememberedRoomTopic(
                channelsHistorySelection.hub_destination_hash,
                channelsHistorySelection.room_name
            ) || null,
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

function _channelsAddLocalRoomItem(roomName, item) {
    var room = String(roomName || '').trim().toLowerCase();
    if (!room || !item || !String(item.text || '').trim()) return;
    var events = _channelsLocalRoomEvents[room] || [];
    var recordedAt = Date.now();
    events.push(Object.assign({
        id: 'local-channel-' + (++_channelsLocalEventSeq),
        kind: 'system',
        timestamp_ms: recordedAt,
        recorded_at_ms: recordedAt,
        source_hash: null,
        nickname: null,
        text: '',
        ours: true,
        mentioned: false
    }, item));
    if (events.length > 20) events.splice(0, events.length - 20);
    _channelsLocalRoomEvents[room] = events;
}

function _channelsAddLocalRoomEvent(roomName, text) {
    _channelsAddLocalRoomItem(roomName, { text: text });
}

function _channelsLiveItemKey(roomName, itemId) {
    var room = String(roomName || '').trim().toLowerCase();
    var id = String(itemId || '').trim();
    return room && id ? room + '\n' + id : '';
}

// Transcript timestamps belong to remote peers and are presentation data, not
// a trustworthy ordering clock. Remember when this client first observes each
// live item so roster-derived presence can be merged without moving messages.
function _channelsObserveLiveItems(rooms) {
    var observedAt = Date.now();
    var retained = {};
    (Array.isArray(rooms) ? rooms : []).forEach(function(room) {
        (room && Array.isArray(room.transcript) ? room.transcript : []).forEach(function(item) {
            var key = _channelsLiveItemKey(room.name, item && item.id);
            if (key) retained[key] = _channelsLiveItemSeenAt[key] || observedAt;
        });
    });
    _channelsLiveItemSeenAt = retained;
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

function _channelsCurrentHubObserved() {
    var destination = String(
        (channelsSnapshot.hub && channelsSnapshot.hub.destination_hash) ||
        channelsSnapshot.selected_hub_destination ||
        ''
    ).toLowerCase();
    var hubs = Array.isArray(channelsSnapshot.hubs) ? channelsSnapshot.hubs : [];
    for (var i = 0; i < hubs.length; i++) {
        if (String(hubs[i].destination_hash || '').toLowerCase() !== destination) continue;
        return hubs[i].observed || null;
    }
    return null;
}

// A presentation-safe view of authenticated native state. This intentionally
// interprets no RRC payloads in JavaScript: WELCOME, directory, and greeting
// provenance have already crossed the native validation boundary as typed
// fields.
function _channelsHubProfileModel() {
    var observed = _channelsCurrentHubObserved();
    var hub = (observed && observed.hub) || channelsSnapshot.hub || {};
    var directory = (observed && observed.directory) ||
        channelsSnapshot.directory || {};
    var greeting = (observed && observed.greeting) ||
        channelsSnapshot.hub_greeting || null;
    var authenticatedName = String(hub.name || '').trim();
    var announcedName = String(hub.announced_name || '').trim();
    var destination = String(hub.destination_hash || '').toLowerCase();
    var directoryRooms = Array.isArray(directory.rooms) ? directory.rooms : [];
    var omitted = Number.isSafeInteger(Number(directory.omitted_count))
        ? Math.max(0, Number(directory.omitted_count)) : 0;
    var directoryPhase = String(directory.phase || 'idle');
    var directorySummary;
    if (directoryPhase === 'loading') {
        directorySummary = 'Refreshing public channels\u2026';
    } else if (directoryPhase === 'error') {
        directorySummary = directory.last_error ||
            'Public channel information is unavailable.';
    } else if (directoryPhase !== 'ready') {
        directorySummary = 'Public channels have not been requested for this Link.';
    } else if (!directoryRooms.length && directory.complete) {
        directorySummary = 'No public channels were advertised.';
    } else {
        directorySummary = directoryRooms.length + ' public ' +
            (directoryRooms.length === 1 ? 'channel' : 'channels');
        if (!directory.complete) {
            directorySummary += omitted
                ? ' shown \u00b7 ' + omitted + ' more omitted by the hub'
                : ' shown \u00b7 response may be incomplete';
        }
    }

    function safeLimit(value) {
        if (value === null || value === undefined || value === '') return null;
        value = Number(value);
        return Number.isSafeInteger(value) && value >= 0 ? value : null;
    }

    var capabilities = hub.capabilities || {};
    var limits = hub.limits || {};
    var phase = String((observed && observed.phase) ||
        channelsSnapshot.phase || 'offline');
    var identityHash = String(hub.identity_hash || '').toLowerCase();
    var connectedAt = safeLimit(hub.connected_at_ms);
    return {
        hub: hub,
        destination_hash: destination,
        identity_hash: identityHash,
        authenticated_name: authenticatedName || null,
        announced_name: announcedName || null,
        display_name: authenticatedName || announcedName ||
            (destination ? 'Channel hub ' + destination.slice(0, 8) : 'Channel hub'),
        name_mismatch: !!(authenticatedName && announcedName &&
            authenticatedName.toLowerCase() !== announcedName.toLowerCase()),
        phase: phase,
        authenticated_session: !!(identityHash && connectedAt != null &&
            (phase === 'active' || phase === 'stale')),
        nickname: (observed && observed.nickname) || channelsSnapshot.nickname || null,
        hops: safeLimit(hub.hops),
        link_mdu: safeLimit(hub.link_mdu),
        connected_at_ms: connectedAt,
        hub_version: hub.version || null,
        protocol_version: channelsSnapshot.protocol_version || null,
        greeting: greeting,
        directory: {
            phase: directoryPhase,
            count: directoryRooms.length,
            complete: !!directory.complete,
            omitted_count: omitted,
            refreshed_at_ms: safeLimit(directory.refreshed_at_ms),
            summary: directorySummary
        },
        capabilities: {
            actions: !!capabilities.actions,
            direct_notices: !!capabilities.direct_notices,
            resource_envelopes: !!capabilities.resource_envelopes,
            rejoin_grace: !!capabilities.rejoin_grace
        },
        limits: {
            max_nick_bytes: safeLimit(limits.max_nick_bytes),
            max_room_name_bytes: safeLimit(limits.max_room_name_bytes),
            max_message_body_bytes: safeLimit(limits.max_message_body_bytes),
            max_rooms_per_session: safeLimit(limits.max_rooms_per_session),
            rate_messages_per_minute: safeLimit(limits.rate_messages_per_minute)
        }
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
    var oldPhase = channelsSnapshot.phase;
    var oldRooms = Array.isArray(channelsSnapshot.rooms) ? channelsSnapshot.rooms : [];
    var oldHub = channelsSnapshot.hub && channelsSnapshot.hub.destination_hash;
    var oldGeneration = Number(channelsSnapshot.generation) || 0;
    var oldContextHub = String(
        channelsSnapshot.selected_hub_destination || oldHub || ''
    ).toLowerCase();
    var incomingHub = snapshot && snapshot.hub && snapshot.hub.destination_hash;
    var incomingContextHub = String(
        snapshot && (snapshot.selected_hub_destination || incomingHub) || ''
    ).toLowerCase();
    if (snapshot && snapshot.phase === 'reconnecting' &&
            (oldPhase === 'active' || oldPhase === 'stale') &&
            oldContextHub && incomingContextHub === oldContextHub) {
        _channelsBeginMemberContinuity(oldContextHub, oldRooms);
    }
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
    var newContextHub = String(
        channelsSnapshot.selected_hub_destination || newHub || ''
    ).toLowerCase();
    var hubContextChanged = newContextHub !== oldContextHub;
    var managerContextChanged = Number(channelsSnapshot.generation) !== oldGeneration;
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
    }
    if (hubContextChanged || managerContextChanged) {
        _channelsLocalRoomEvents = {};
        _channelsLiveItemSeenAt = {};
        _channelsSelectedMemberKey = null;
        _channelsMemberReturnFocusKey = null;
        _channelsResetMemberObservations();
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
    _channelsObserveLiveItems(channelsSnapshot.rooms);
    _channelsObserveRoomMembers(newContextHub, channelsSnapshot.rooms);

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
            if (entry.hub_destination_hash !== destination) return;
            localByRoom[entry.room_name] = Object.assign({}, entry);
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
            marked_sequence: '0',
            participants: [],
            participants_loaded: false,
            participants_loading: false,
            participants_error: null,
            participants_omitted: 0,
            participants_request_id: 0,
            participants_refresh_requested: false
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
        source_lxmf_hash: item.source_lxmf_hash || null,
        nickname: item.nickname || null,
        text: String(item.text || ''),
        ours: !!item.ours,
        mentioned: !!item.mentioned
    };
}

function _channelsNormalizeParticipant(item) {
    item = item || {};
    var identityHash = String(item.identity_hash || '').trim().toLowerCase();
    var lxmfHash = String(item.lxmf_hash || '').trim().toLowerCase();
    var nickname = String(item.nickname || '').trim();
    if (!/^[0-9a-f]{32}$/.test(identityHash)) identityHash = '';
    if (!/^[0-9a-f]{32}$/.test(lxmfHash)) lxmfHash = '';
    if (/[\u0000-\u001f\u007f]/.test(nickname)) nickname = '';
    if (!identityHash && !nickname) return null;
    return {
        identity_hash: identityHash || null,
        lxmf_hash: lxmfHash || null,
        nickname: nickname || null,
        last_seen_at_ms: Number(item.last_seen_at_ms) || 0,
        is_self: false,
        _seen: true
    };
}

function _channelsLoadParticipants(context, force) {
    var entry = _channelsHistoryEntry(context);
    if (!entry) return Promise.resolve(entry);
    if (entry.participants_loading) {
        if (force) entry.participants_refresh_requested = true;
        return Promise.resolve(entry);
    }
    if (!force && (entry.participants_loaded || entry.participants_error)) {
        return Promise.resolve(entry);
    }
    var requestId = ++_channelsParticipantRequestSeq;
    var requestEpoch = _channelsHistoryEpoch;
    entry.participants_loading = true;
    entry.participants_refresh_requested = false;
    entry.participants_error = null;
    entry.participants_request_id = requestId;
    return RS.invoke('api_channel_participants', {
        args: {
            hub_destination_hash: context.hub_destination_hash,
            room: context.room_name
        }
    }).then(function(page) {
        if (requestEpoch !== _channelsHistoryEpoch ||
                entry.participants_request_id !== requestId) return entry;
        page = page || {};
        entry.participants = (Array.isArray(page.participants) ? page.participants : [])
            .map(_channelsNormalizeParticipant)
            .filter(Boolean);
        entry.participants_omitted = Math.max(0, Number(page.omitted_count) || 0);
        entry.participants_loaded = true;
        entry.participants_error = null;
        return entry;
    }).catch(function(error) {
        if (requestEpoch === _channelsHistoryEpoch &&
                entry.participants_request_id === requestId) {
            entry.participants_error = 'Seen participants could not be loaded.';
            window.RS.diag('warn', '[Channels] participant summary query failed:', error);
        }
        return entry;
    }).then(function(result) {
        if (requestEpoch !== _channelsHistoryEpoch ||
                entry.participants_request_id !== requestId) return result;
        entry.participants_loading = false;
        var refreshRequested = entry.participants_refresh_requested;
        entry.participants_refresh_requested = false;
        if (_channelsCurrentHistoryKey() === context.key) {
            var room = channelsActiveRoom ? _channelsRoomByName(channelsActiveRoom) : null;
            _channelsRenderMembers(room);
        }
        if (refreshRequested) return _channelsLoadParticipants(context, true);
        return result;
    });
}

function _channelsEnsureParticipants(context) {
    var entry = _channelsHistoryEntry(context);
    if (entry && !entry.participants_loaded && !entry.participants_loading &&
            !entry.participants_error) {
        _channelsLoadParticipants(context, false);
    }
    return entry;
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
        if (changed) {
            _channelsLoadParticipants(context, true);
            if (_channelsCurrentHistoryKey() === context.key) _channelsRenderRoom();
        }
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
        var topic = _channelsRememberedRoomTopic(destination, room.name, room.topic);
        var roomFingerprint = topic || '';
        if (_channelsSavedRoomKeys[roomKey] === roomFingerprint) return;
        _channelsSavedRoomKeys[roomKey] = roomFingerprint;
        Promise.resolve(_channelsSaveHubPromise).then(function() {
            return RS.invoke('save_channel_room', {
                args: {
                    hub_destination_hash: destination,
                    room: room.name,
                    topic: topic || null,
                    joined: true
                }
            });
        }).then(function() {
            return channelsRefreshRoomIndex();
        }).then(function() {
            _channelsSavedRoomsHub = null;
            return channelsLoadSavedRooms(destination);
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
}

function _channelsHubPulseEnabled() {
    return !(typeof window !== 'undefined' && window.matchMedia &&
        window.matchMedia('(prefers-reduced-motion: reduce)').matches);
}

function _channelsPlayHubSignal(strip) {
    if (!strip || !_channelsHubPulseEnabled()) return;
    strip.classList.remove('link-arrived');
    requestAnimationFrame(function() {
        if (strip.dataset.phase === 'active') strip.classList.add('link-arrived');
    });
}

function _channelsStopHubPulse(strip) {
    if (_channelsHubPulseTimer != null) {
        clearInterval(_channelsHubPulseTimer);
        _channelsHubPulseTimer = null;
    }
    if (strip) strip.classList.remove('link-arrived');
}

function _channelsSyncHubPulse(strip, phase) {
    if (phase !== 'active' || !_channelsHubPulseEnabled()) {
        _channelsStopHubPulse(strip);
        return;
    }
    if (_channelsHubPulseTimer != null) return;
    _channelsHubPulseTimer = setInterval(function() {
        if (channelsSnapshot.phase === 'active' && _channelsViewVisible()) {
            _channelsPlayHubSignal(strip);
        }
    }, CHANNEL_HUB_PULSE_INTERVAL_MS);
}

function _channelsRenderHubStrip() {
    var strip = _channelsEl('channel-hub-strip');
    var summary = _channelsEl('channel-hub-summary');
    var menu = _channelsEl('channel-hub-menu-btn');
    if (!strip) return;

    var previousPhase = strip.dataset.phase || '';
    var nextPhase = channelsSnapshot.phase || 'unavailable';
    strip.dataset.phase = nextPhase;
    if (nextPhase === 'active' && previousPhase !== 'active') {
        _channelsPlayHubSignal(strip);
    }
    _channelsSyncHubPulse(strip, nextPhase);
    if (menu) menu.hidden = false;

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
    if (summary) {
        var summaryAction = hub ? 'Hub options' : 'Manage Hub';
        summary.title = summaryAction;
        summary.setAttribute('aria-haspopup', hub ? 'menu' : 'dialog');
        summary.setAttribute('aria-label', summaryAction + '. ' + title + '. ' + meta);
    }
}

function _channelsRenderList() {
    var list = _channelsEl('channels-list');
    var label = _channelsEl('channels-list-label');
    var join = _channelsEl('channels-join-btn');
    if (!list) return;
    list.textContent = '';

    if (_channelsIsConnected()) {
        if (label) label.textContent = 'Active';
        if (join) {
            join.hidden = false;
            join.textContent = 'Join';
            join.setAttribute('aria-label', 'Join a channel on this hub');
        }
        var liveNames = {};
        channelsSnapshot.rooms.forEach(function(room) {
            liveNames[room.name] = true;
        });
        var savedOnlyRooms = channelsSavedRooms.filter(function(saved) {
            return !liveNames[saved.room_name];
        });
        channelsSnapshot.rooms.forEach(function(room) {
            list.appendChild(_channelsBuildRoomRow(room, false));
        });
        if (!channelsSnapshot.rooms.length) {
            list.appendChild(_channelsActiveEmpty('No currently active channels'));
        }
        if (savedOnlyRooms.length) {
            list.appendChild(_channelsListSection('History', {
                className: 'channel-history-section'
            }));
        }
        savedOnlyRooms.forEach(function(saved) {
            var indexed = channelsRoomIndex.find(function(entry) {
                return entry.hub_destination_hash === saved.hub_destination_hash &&
                    entry.room_name === saved.room_name;
            });
            list.appendChild(_channelsBuildRoomRow({ name: saved.room_name }, true, {
                hub_destination_hash: saved.hub_destination_hash,
                has_history: !!(indexed && indexed.has_history),
                topic: indexed && indexed.topic
            }));
        });

        var directory = channelsSnapshot.directory || {};
        var directoryRooms = Array.isArray(directory.rooms) ? directory.rooms : [];
        var knownNames = Object.assign({}, liveNames);
        savedOnlyRooms.forEach(function(saved) { knownNames[saved.room_name] = true; });
        var availableRooms = directoryRooms.filter(function(room) {
            return room && room.name && !knownNames[room.name];
        });
        list.appendChild(_channelsListSection('Discover', {
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
            list.appendChild(_channelsDirectoryStatus('No discoverable channels found.'));
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
                topic: saved.topic,
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

function _channelsActiveEmpty(text) {
    var empty = document.createElement('div');
    empty.className = 'channel-active-empty';
    empty.textContent = text;
    return empty;
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
    row.setAttribute('aria-label', 'Join ' + _channelsRoomDisplayName(room.name));

    var icon = document.createElement('span');
    icon.className = 'channel-room-row-icon';
    icon.innerHTML = _channelsRoomIcon();
    row.appendChild(icon);

    var copy = document.createElement('span');
    copy.className = 'channel-room-row-copy';
    var title = document.createElement('span');
    title.className = 'channel-room-row-title';
    title.textContent = _channelsRoomDisplayName(room.name);
    var meta = document.createElement('span');
    meta.className = 'channel-room-row-meta';
    meta.textContent = room.topic || 'Public channel on this hub';
    copy.appendChild(title);
    copy.appendChild(meta);
    row.appendChild(copy);

    var action = document.createElement('span');
    action.className = 'channel-row-add';
    action.textContent = '+';
    action.setAttribute('aria-hidden', 'true');
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
    row.className = 'channel-room-row' + (savedOnly ? ' channel-room-row-history' : '') + (
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
    title.textContent = _channelsRoomDisplayName(room.name);
    var meta = document.createElement('span');
    meta.className = 'channel-room-row-meta';
    if (savedOnly) {
        meta.textContent = _channelsRememberedRoomTopic(
            roomHub,
            room.name,
            options.topic || room.topic
        ) || (options.hub_label || (options.has_history ? 'Disconnected' : 'Saved channel'));
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

    if ((!savedOnly && room.phase !== 'joined') || (savedOnly && !options.has_history)) {
        var status = document.createElement('span');
        status.className = 'channel-row-status' + (!savedOnly && room.phase === 'error' ? ' error' : '');
        status.textContent = savedOnly ? 'Saved' : _channelsRoomPhaseLabel(room.phase);
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
    if (RS.chatScroll) RS.chatScroll.wire(transcript);
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
    _channelsEnsureParticipants(historyContext);
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
    _channelsSetText('channel-room-title', _channelsRoomDisplayName(room.name));
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
        roomMeta = _channelsRememberedRoomTopic(
            historyContext.hub_destination_hash,
            room.name,
            room.topic
        ) || _channelsTimelineHubName(_channelsHubByDestination(
            historyContext.hub_destination_hash
        ));
    } else {
        roomMeta = room.topic || (memberCount
            ? memberCount + (memberCount === 1 ? ' person here' : ' people here')
            : _channelsHubName(channelsSnapshot.hub));
    }
    _channelsSetText('channel-room-meta', roomMeta);

    var scrollState = RS.chatScroll
        ? RS.chatScroll.capture(transcript)
        : { scrollTop: transcript.scrollTop, nearBottom: true, followLatest: true };
    transcript.textContent = '';
    if (!room.history_only) {
        channelsSnapshot.notices.forEach(function(item) {
            transcript.appendChild(_channelsBuildHubNotice(item));
        });
    }
    if (historyContext && historyEntry) {
        var historyRail = _channelsBuildHistoryRail(historyContext, historyEntry);
        if (historyRail) transcript.appendChild(historyRail);
    }
    var items = _channelsTimelineEntries(room, historyEntry);
    var renderedItems = _channelsGroupConsecutiveMessages(items);
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
            var day = _channelsDayKey(entry.item);
            if (day && day !== previousDay) {
                transcript.appendChild(_channelsBuildDaySeparator(entry.item));
                previousDay = day;
            }
            transcript.appendChild(_channelsBuildTranscriptItem(
                entry.item,
                entry.hubNotice,
                entry.messageGroup
            ));
        });
    }
    if (scrollRestore && scrollRestore.key === (historyContext && historyContext.key)) {
        var restoredTop = scrollRestore.scroll_top +
            Math.max(0, transcript.scrollHeight - scrollRestore.scroll_height);
        if (RS.chatScroll) RS.chatScroll.setTop(transcript, restoredTop);
        else transcript.scrollTop = restoredTop;
    } else if (RS.chatScroll) {
        RS.chatScroll.applyAfterRender(transcript, scrollState, {
            forceScrollBottom: roomChanged
        });
    } else if (scrollState.nearBottom || roomChanged) {
        transcript.scrollTop = transcript.scrollHeight;
    } else {
        transcript.scrollTop = scrollState.scrollTop;
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
        if (_channelsIsBlockedItem(item)) return;
        if (_channelsIsConnectionLifecycleItem(item)) return;
        // RRC membership is best-effort and many hubs omit member lists or
        // cannot prove every disconnect. Keep JOINED/PARTED as native state
        // evidence for the people pane without presenting an asymmetric
        // activity feed as conversation. Our own join remains a useful local
        // session boundary.
        if (_channelsIsRemotePresenceItem(item)) return;
        var id = String(item.id || '');
        if (id && seen[id]) return;
        if (id) seen[id] = true;
        entries.push({ item: item, hubNotice: !!hubNotice, order: order++ });
    }

    if (room.history_only) {
        historyItems.forEach(function(item) { append(item, false); });
        return _channelsOrderTimelineEntries(entries);
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
        var observedAt = _channelsLiveItemSeenAt[
            _channelsLiveItemKey(room.name, item && item.id)
        ];
        var merged = stored ? Object.assign({}, item, {
            sequence: stored.sequence,
            recorded_at_ms: stored.recorded_at_ms || observedAt || 0,
            mentioned: !!(item.mentioned || stored.mentioned)
        }) : Object.assign({}, item, {
            recorded_at_ms: observedAt || 0
        });
        append(merged, _channelsIsHubNotice(merged));
    });
    (_channelsLocalRoomEvents[room.name] || []).forEach(function(item) {
        append(item, false);
    });
    return _channelsOrderTimelineEntries(entries);
}

function _channelsBuildHistoryRail(context, entry) {
    var writer = channelsSnapshot.history || {};
    var statusText = '';
    if (entry.error) {
        statusText = entry.error;
    } else if (!entry.loaded && entry.loading) {
        statusText = 'Loading recent activity\u2026';
    } else if (writer.phase === 'degraded') {
        statusText = writer.last_error || 'Some recent activity could not be saved.';
    } else if (writer.phase === 'pending' && Number(writer.pending_events) > 0) {
        statusText = 'Saving recent activity\u2026';
    }
    if (!statusText && !entry.has_more) return null;

    var rail = document.createElement('div');
    rail.className = 'channel-history-rail' + (statusText ? '' : ' action-only');
    rail.dataset.phase = entry.error ? 'error' :
        (channelsSnapshot.history && channelsSnapshot.history.phase) || 'ready';
    rail.setAttribute('role', entry.error ? 'alert' : 'status');

    if (statusText) {
        var label = document.createElement('span');
        label.className = 'channel-history-label';
        label.textContent = statusText;
        rail.appendChild(label);
    }

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
    copy.textContent = 'Not currently joined.';
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
        title.textContent = 'Loading history\u2026';
        copy.textContent = 'Recent messages will appear in a moment.';
    } else if (entry && entry.error) {
        title.textContent = 'History is temporarily unavailable';
        copy.textContent = 'Try again in a moment.';
    } else {
        title.textContent = 'No messages yet';
        copy.textContent = 'Messages you receive in this channel will appear here.';
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
    var secondary = null;
    var hubGreeting = null;

    if (_channelsIsConnecting()) {
        title.textContent = _channelsPhaseLabel(channelsSnapshot.phase) + '\u2026';
        copy.textContent = 'Ratspeak is establishing an authenticated Reticulum Link and waiting for the hub to welcome this session.';
        button.className = 'nr-btn nr-btn-secondary';
        button.dataset.channelAction = 'disconnect';
        button.textContent = 'Cancel';
    } else if (_channelsIsConnected()) {
        var profile = _channelsHubProfileModel();
        state.classList.add('channel-hub-home-state');
        title.textContent = profile.display_name;
        copy.textContent = 'Connected' +
            (profile.hops == null
                ? ''
                : ' \u00b7 ' + profile.hops +
                    (profile.hops === 1 ? ' hop' : ' hops')) +
            ' \u00b7 ' + profile.directory.summary;
        button.dataset.channelAction = 'join';
        button.textContent = 'Browse channels';
        secondary = document.createElement('button');
        secondary.type = 'button';
        secondary.className = 'nr-btn nr-btn-secondary';
        secondary.dataset.channelAction = 'hub-info';
        secondary.textContent = 'Hub information';
        if (profile.greeting) {
            hubGreeting = _channelsBuildHubGreeting(profile.greeting);
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
    var actions = document.createElement('div');
    actions.className = 'channel-welcome-actions';
    actions.appendChild(button);
    if (secondary) actions.appendChild(secondary);
    state.appendChild(actions);
    if (_channelsIsConnected()) {
        var home = document.createElement('div');
        home.className = 'channel-hub-home';
        home.appendChild(state);
        if (hubGreeting) home.appendChild(hubGreeting);
        transcript.appendChild(home);
    } else {
        transcript.appendChild(state);
    }
}

function _channelsBuildHubGreeting(item, compact) {
    var greeting = document.createElement('aside');
    greeting.className = 'channel-hub-greeting' + (compact ? ' compact' : '');
    greeting.dataset.delivery = item.delivery || 'notice';
    greeting.dataset.completeness = item.completeness || 'unframed';
    greeting.setAttribute(
        'aria-label',
        'Welcome and guidance from the authenticated channel hub'
    );
    var heading = document.createElement('div');
    heading.className = 'channel-hub-greeting-heading';
    var label = document.createElement('span');
    label.className = 'channel-hub-notice-label';
    label.textContent = 'Hub welcome';
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

// Recovery is a Link state, not room activity. Keep legacy persisted markers
// out of the human timeline while older local databases age them out normally.
function _channelsIsConnectionLifecycleItem(item) {
    return !!item && item.kind === 'system' && item.text === 'Reconnected to hub';
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
    time.dateTime = _channelsDisplayDate(item.timestamp_ms).toISOString();
    time.textContent = _channelsFormatTime(item.timestamp_ms);
    var meta = document.createElement('span');
    meta.className = 'channel-event-meta';
    var quote = _channelsBuildQuoteButton(item, 'Hub');
    if (quote) meta.appendChild(quote);
    meta.appendChild(time);
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

function _channelsBindTouchReplyAction(event, item, authorText) {
    if (!event || !window.RS || !RS.gestures ||
            typeof RS.gestures.attachLongPress !== 'function') return;
    RS.gestures.attachLongPress(event, {
        duration: 500,
        moveCancelPx: 12,
        hapticStages: [{ at: 0.55, level: 'light' }],
        preventDefaultOnStart: true,
        onFire: function() {
            if (typeof actionPopover !== 'function') {
                _channelsInsertQuote(item, authorText);
                return;
            }
            var actions = [{
                label: 'Quote in reply',
                icon: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="9 17 4 12 9 7"></polyline><path d="M20 18v-2a4 4 0 0 0-4-4H4"></path></svg>',
                onSelect: function() { _channelsInsertQuote(item, authorText); }
            }];
            if (!item.ours) {
                actions.push({
                    label: 'Report',
                    icon: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M5 21V4"/><path d="M5 4h11l-1 4 1 4H5"/></svg>',
                    onSelect: function() {
                        _channelsOpenReportSheet({
                            nickname: authorText,
                            identityHash: item.source_hash,
                            lxmfAddress: item.source_lxmf_hash,
                            messageId: item.id,
                            messageText: item.text,
                            timestampMs: item.timestamp_ms,
                            room: channelsActiveRoom
                        });
                    }
                });
            }
            actionPopover(event, actions);
        }
    });
}

function _channelsIsRemotePresenceItem(item) {
    if (!item || item.ours) return false;
    return item.kind === 'join' || item.kind === 'part' || item.kind === 'present';
}

// Native history is already append-ordered. Merge the bounded live window and
// local-only feedback by their trusted local receive clock so a rerender can
// never move an older event below a newer message.
function _channelsOrderTimelineEntries(entries) {
    return (entries || []).map(function(entry, index) {
        return {
            entry: entry,
            index: index,
            // Only the local receive clock participates in ordering. A missing
            // value stays in canonical append order after recorded entries.
            time: Number(entry && entry.item && entry.item.recorded_at_ms) ||
                Number.MAX_SAFE_INTEGER
        };
    }).sort(function(left, right) {
        return left.time - right.time || left.index - right.index;
    }).map(function(ordered) {
        return ordered.entry;
    });
}

function _channelsMessageAuthorKey(entry) {
    if (!entry || entry.hubNotice || !entry.item ||
            (entry.item.kind || 'message') !== 'message') return '';
    if (entry.item.ours) return 'self';
    var sourceHash = String(entry.item.source_hash || '').trim().toLowerCase();
    if (sourceHash) return 'source:' + sourceHash;
    var nickname = String(entry.item.nickname || '').trim().toLowerCase();
    return nickname ? 'nickname:' + nickname : '';
}

function _channelsMessagesBelongTogether(previous, current) {
    var previousAuthor = _channelsMessageAuthorKey(previous);
    if (!previousAuthor || previousAuthor !== _channelsMessageAuthorKey(current)) return false;
    if (!!previous.item.mentioned !== !!current.item.mentioned) return false;
    if (_channelsDayKey(previous.item) !== _channelsDayKey(current.item)) return false;
    var elapsed = _channelsActivityTime(current.item) - _channelsActivityTime(previous.item);
    return elapsed >= 0 && elapsed <= CHANNEL_MESSAGE_GROUP_WINDOW_MS;
}

function _channelsGroupConsecutiveMessages(entries) {
    return (entries || []).map(function(entry, index, allEntries) {
        var joinsPrevious = index > 0 &&
            _channelsMessagesBelongTogether(allEntries[index - 1], entry);
        var joinsNext = index + 1 < allEntries.length &&
            _channelsMessagesBelongTogether(entry, allEntries[index + 1]);
        var messageGroup = 'single';
        if (!joinsPrevious && joinsNext) messageGroup = 'start';
        else if (joinsPrevious && joinsNext) messageGroup = 'middle';
        else if (joinsPrevious) messageGroup = 'end';
        return Object.assign({}, entry, { messageGroup: messageGroup });
    });
}

function _channelsBuildTranscriptItem(item, hubNotice, messageGroup) {
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
        (item.ours ? ' ours' : '') + (mentioned ? ' mentioned' : '') +
        (messageGroup && messageGroup !== 'single'
            ? ' message-group-' + messageGroup
            : '');
    var authorText = item.nickname || (item.ours ? (channelsSnapshot.nickname || 'You') : _channelsShortHash(item.source_hash)) || 'Hub';

    var avatar = document.createElement('span');
    avatar.className = 'channel-event-avatar';
    _channelsPopulateIdentityAvatar(
        avatar,
        _channelsIdentityAvatarSeed(item.source_hash, item.source_lxmf_hash, !!item.ours),
        32,
        authorText
    );

    var author = document.createElement('span');
    author.className = 'channel-event-author';
    var authorLabel = document.createElement('span');
    authorLabel.textContent = item.ours ? authorText + ' (you)' : authorText;
    author.appendChild(authorLabel);
    if (mentioned) {
        var mentionMarker = document.createElement('span');
        mentionMarker.className = 'channel-mention-marker';
        mentionMarker.textContent = 'Mention';
        author.appendChild(mentionMarker);
    }
    var time = document.createElement('time');
    time.className = 'channel-event-time';
    time.dateTime = _channelsDisplayDate(item.timestamp_ms).toISOString();
    time.textContent = _channelsFormatTime(item.timestamp_ms);
    var meta = document.createElement('span');
    meta.className = 'channel-event-meta';
    var quote = _channelsBuildQuoteButton(item, authorText);
    if (quote) meta.appendChild(quote);
    if (!item.ours) {
        var report = document.createElement('button');
        report.type = 'button';
        report.className = 'channel-event-report';
        report.title = 'Report message';
        report.setAttribute('aria-label', 'Report message from ' + authorText);
        report.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M5 21V4"/><path d="M5 4h11l-1 4 1 4H5"/></svg>';
        report.addEventListener('click', function(event) {
            event.preventDefault();
            event.stopPropagation();
            _channelsOpenReportSheet({
                nickname: authorText,
                identityHash: item.source_hash,
                lxmfAddress: item.source_lxmf_hash,
                messageId: item.id,
                messageText: item.text,
                timestampMs: item.timestamp_ms,
                room: channelsActiveRoom
            });
        });
        meta.appendChild(report);
    }
    meta.appendChild(time);
    var body = document.createElement('div');
    body.className = 'channel-event-text';
    body.textContent = kind === 'action' ? authorText + ' ' + (item.text || '') : (item.text || '');

    event.appendChild(avatar);
    event.appendChild(author);
    event.appendChild(meta);
    event.appendChild(body);
    _channelsBindTouchReplyAction(event, item, authorText);
    return event;
}

function _channelsMemberName(member) {
    return member.nickname || _channelsShortHash(member.identity_hash) || 'Channel member';
}

function _channelsMemberListName(member) {
    var channelNickname = String(member.nickname || '').trim();
    if (channelNickname) return channelNickname;
    var details = _channelsMemberDetails(member);
    return details.knownName || _channelsShortHash(member.identity_hash) || 'Channel member';
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

function _channelsObservedRoom(hubDestinationHash, roomName) {
    var key = _channelsHistoryKey(hubDestinationHash, roomName);
    if (!key) return null;
    if (!_channelsObservedMembersByRoom[key]) {
        _channelsObservedMembersByRoom[key] = { members: {} };
    }
    return _channelsObservedMembersByRoom[key];
}

function _channelsResetMemberObservations() {
    _channelsObservedMembersByRoom = {};
    if (_channelsMemberContinuityTimer != null) {
        clearTimeout(_channelsMemberContinuityTimer);
        _channelsMemberContinuityTimer = null;
    }
}

function _channelsScheduleMemberContinuity() {
    if (_channelsMemberContinuityTimer != null) {
        clearTimeout(_channelsMemberContinuityTimer);
        _channelsMemberContinuityTimer = null;
    }
    var now = Date.now();
    var earliest = 0;
    Object.keys(_channelsObservedMembersByRoom).forEach(function(roomKey) {
        var observed = _channelsObservedMembersByRoom[roomKey];
        Object.keys(observed.members).forEach(function(memberKey) {
            var deadline = Number(observed.members[memberKey].continuity_until_ms) || 0;
            if (deadline > now && (!earliest || deadline < earliest)) earliest = deadline;
        });
    });
    if (!earliest) return;
    _channelsMemberContinuityTimer = setTimeout(function() {
        _channelsMemberContinuityTimer = null;
        var room = channelsActiveRoom ? _channelsRoomByName(channelsActiveRoom) : null;
        if (room) _channelsRenderMembers(room);
        _channelsScheduleMemberContinuity();
    }, Math.max(1, earliest - now + 20));
}

function _channelsBeginMemberContinuity(hubDestinationHash, rooms) {
    var deadline = Date.now() + CHANNEL_MEMBER_CONTINUITY_MS;
    (Array.isArray(rooms) ? rooms : []).forEach(function(room) {
        if (!room || room.phase !== 'joined') return;
        var observed = _channelsObservedRoom(hubDestinationHash, room.name);
        if (!observed) return;
        (Array.isArray(room.members) ? room.members : []).forEach(function(member) {
            if (!member || member.is_self) return;
            var key = _channelsMemberKey(member);
            var record = observed.members[key] || {};
            record.member = Object.assign({}, member, { is_self: false });
            record.last_visible_at_ms = Date.now();
            record.continuity_until_ms = deadline;
            observed.members[key] = record;
        });
    });
    _channelsScheduleMemberContinuity();
}

function _channelsObserveRoomMembers(hubDestinationHash, rooms) {
    (Array.isArray(rooms) ? rooms : []).forEach(function(room) {
        if (!room || room.phase !== 'joined') return;
        var observed = _channelsObservedRoom(hubDestinationHash, room.name);
        if (!observed) return;
        (Array.isArray(room.transcript) ? room.transcript : []).forEach(function(item) {
            if (!item || item.kind !== 'part' || item.ours) return;
            var key = _channelsMemberKey({
                identity_hash: item.source_hash,
                nickname: item.nickname
            });
            if (observed.members[key]) {
                observed.members[key].continuity_until_ms = 0;
            }
        });
        (Array.isArray(room.members) ? room.members : []).forEach(function(member) {
            if (!member || member.is_self) return;
            var key = _channelsMemberKey(member);
            var record = observed.members[key] || {};
            record.member = Object.assign({}, member, { is_self: false });
            record.last_visible_at_ms = Date.now();
            record.continuity_until_ms = 0;
            observed.members[key] = record;
        });
    });
}

function _channelsMemberRosterModel(room) {
    var context = _channelsHistoryContext(room);
    var nativeMembers = room && Array.isArray(room.members) ? room.members : [];
    var visible = nativeMembers.filter(function(member) {
        return !_channelsIsBlockedMember(member);
    });
    var visibleKeys = {};
    var visibleNames = {};
    visible.forEach(function(member) {
        visibleKeys[_channelsMemberKey(member)] = true;
        var name = String(member.nickname || '').trim().toLowerCase();
        if (name) visibleNames[name] = true;
    });

    var continuity = [];
    var now = Date.now();
    var observed = context ? _channelsObservedMembersByRoom[context.key] : null;
    if (observed) {
        Object.keys(observed.members).forEach(function(key) {
            var record = observed.members[key];
            if (visibleKeys[key] || !record.member || record.member.is_self) return;
            var deadline = Number(record.continuity_until_ms) || 0;
            if (deadline <= now || room.members_complete) {
                record.continuity_until_ms = 0;
                return;
            }
            var member = Object.assign({}, record.member, {
                _continuity: true,
                last_seen_at_ms: Number(record.last_visible_at_ms) || 0
            });
            continuity.push(member);
            visibleKeys[key] = true;
            var name = String(member.nickname || '').trim().toLowerCase();
            if (name) visibleNames[name] = true;
        });
    }

    var seenByKey = {};
    var historyEntry = context ? _channelsHistoryEntry(context) : null;
    var retained = historyEntry && Array.isArray(historyEntry.participants)
        ? historyEntry.participants : [];
    var identifiedSeenNames = {};
    retained.forEach(function(member) {
        var name = String(member.nickname || '').trim().toLowerCase();
        if (member.identity_hash && name) identifiedSeenNames[name] = true;
    });
    retained.forEach(function(member) {
        var key = _channelsMemberKey(member);
        var name = String(member.nickname || '').trim().toLowerCase();
        if (visibleKeys[key] || (name && visibleNames[name])) return;
        // An unresolved nickname event cannot add trustworthy identity detail.
        // Prefer an identified observation with the exact same display name
        // instead of presenting an apparent duplicate person after reload.
        if (!member.identity_hash && name && identifiedSeenNames[name]) return;
        seenByKey[key] = Object.assign({}, member, { _seen: true });
    });
    if (observed) {
        Object.keys(observed.members).forEach(function(key) {
            var record = observed.members[key];
            if (visibleKeys[key] || !record.member || record.member.is_self) return;
            var name = String(record.member.nickname || '').trim().toLowerCase();
            if (name && visibleNames[name]) return;
            if (!record.member.identity_hash && name && identifiedSeenNames[name]) return;
            if (record.member.identity_hash && name) {
                identifiedSeenNames[name] = true;
                delete seenByKey['nickname:' + name];
            }
            var existing = seenByKey[key];
            if (!existing || Number(record.last_visible_at_ms) > Number(existing.last_seen_at_ms)) {
                seenByKey[key] = Object.assign({}, record.member, {
                    _seen: true,
                    last_seen_at_ms: Number(record.last_visible_at_ms) || 0
                });
            }
        });
    }
    var seen = Object.keys(seenByKey).map(function(key) { return seenByKey[key]; });
    continuity = continuity.filter(function(member) {
        return !_channelsIsBlockedMember(member);
    });
    seen = seen.filter(function(member) {
        return !_channelsIsBlockedMember(member);
    });
    seen.sort(function(a, b) {
        return Number(b.last_seen_at_ms || 0) - Number(a.last_seen_at_ms || 0);
    });
    continuity.sort(function(a, b) {
        return Number(b.last_seen_at_ms || 0) - Number(a.last_seen_at_ms || 0);
    });
    return {
        visible: visible,
        continuity: continuity,
        seen: seen,
        omitted: historyEntry ? Number(historyEntry.participants_omitted) || 0 : 0
    };
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

function _channelsIdentityAvatarSeed(identityHash, lxmfHash, isSelf) {
    var target = String(identityHash || '').trim().toLowerCase();
    var canonical = String(lxmfHash || '').trim().toLowerCase();
    if (canonical) return canonical;
    if (isSelf) {
        var active = typeof activeIdentity === 'function' ? activeIdentity() : null;
        var activeIdentityHash = String(active && (active.hash || active.identity_hash) || '')
            .trim().toLowerCase();
        if (active && (!target || !activeIdentityHash || activeIdentityHash === target)) {
            var activeLxmf = String(active.lxmf_hash || '').trim().toLowerCase();
            if (activeLxmf) return activeLxmf;
        }
        var live = typeof lxmfIdentity !== 'undefined' ? lxmfIdentity : null;
        var liveIdentityHash = String(live && live.identity_hash || '').trim().toLowerCase();
        if (live && (!target || !liveIdentityHash || liveIdentityHash === target)) {
            var liveLxmf = String(live.lxmf_hash || live.hash || '').trim().toLowerCase();
            if (liveLxmf) return liveLxmf;
        }
    } else if (target) {
        var peerLxmf = _channelsPeerLxmfAddress(_channelsPeerForIdentity(target));
        if (peerLxmf) return String(peerLxmf).trim().toLowerCase();
    }
    return '';
}

function _channelsAvatarFallbackLabel(value) {
    var label = String(value || '').trim();
    return label ? Array.from(label)[0].toUpperCase() : '';
}

function _channelsPopulateIdentityAvatar(element, seed, size, fallbackLabel) {
    element.setAttribute('aria-hidden', 'true');
    if (seed && typeof identityAvatar === 'function') {
        element.innerHTML = identityAvatar(seed, size);
        return;
    }
    var fallback = document.createElement('span');
    fallback.className = 'channel-avatar-fallback';
    fallback.textContent = _channelsAvatarFallbackLabel(fallbackLabel);
    element.appendChild(fallback);
}

function _channelsMemberDetails(member) {
    var identityHash = String(member.identity_hash || '').toLowerCase();
    var peer = member.is_self ? null : _channelsPeerForIdentity(identityHash);
    var active = member.is_self && typeof activeIdentity === 'function' ? activeIdentity() : null;
    var activeMatches = active && (!identityHash || String(active.hash || '').toLowerCase() === identityHash);
    var liveSelf = member.is_self && typeof lxmfIdentity !== 'undefined' ? lxmfIdentity : null;
    var liveSelfMatches = liveSelf && (!identityHash || String(liveSelf.identity_hash || '').toLowerCase() === identityHash);
    var lxmfAddress = member.is_self
        ? String(member.lxmf_hash || (activeMatches ? active.lxmf_hash : '') || (liveSelfMatches ? liveSelf.hash : '') || '')
        : String(member.lxmf_hash || _channelsPeerLxmfAddress(peer) || '');
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

function _channelsOpenReportSheet(context) {
    context = context || {};
    if (typeof _rsBuildSheet !== 'function') return;
    var roomName = String(context.room || channelsActiveRoom || '').trim();
    var hub = channelsSnapshot.hub || {};
    var hubName = _channelsHubName(hub);
    var hubAddress = String(hub.destination_hash || '').trim().toLowerCase();
    var nickname = String(context.nickname || 'Channel member').trim();
    var identityHash = String(context.identityHash || '').trim().toLowerCase();
    var lxmfAddress = String(context.lxmfAddress || '').trim().toLowerCase();
    var messageText = String(context.messageText || '').trim();
    var messageId = String(context.messageId || '').trim();
    var timestampMs = Number(context.timestampMs) || 0;

    var built = _rsBuildSheet({ title: 'Report channel content' }, function() {});
    built.sheet.classList.add('channel-report-sheet');
    var explanation = document.createElement('p');
    explanation.className = 'channel-report-explanation';
    explanation.textContent = 'Ratspeak can investigate app conduct and activity on official Ratspeak services. Independent hub operators control and moderate their own hubs.';
    built.body.appendChild(explanation);

    var summary = document.createElement('div');
    summary.className = 'channel-report-summary';
    [
        ['Hub', hubName],
        ['Channel', roomName ? _channelsRoomDisplayName(roomName) : 'Unknown'],
        ['Reported person', nickname]
    ].forEach(function(field) {
        var row = document.createElement('div');
        var label = document.createElement('span');
        label.textContent = field[0];
        var value = document.createElement('strong');
        value.textContent = field[1];
        row.appendChild(label);
        row.appendChild(value);
        summary.appendChild(row);
    });
    if (messageText) {
        var excerpt = document.createElement('blockquote');
        excerpt.textContent = messageText.length > 500
            ? messageText.slice(0, 500) + '\u2026'
            : messageText;
        summary.appendChild(excerpt);
    }
    built.body.appendChild(summary);

    var privacy = document.createElement('p');
    privacy.className = 'channel-report-privacy';
    privacy.textContent = 'Nothing is sent automatically. Ratspeak will prepare an email containing the details above for you to review.';
    built.body.appendChild(privacy);

    var error = document.createElement('div');
    error.className = 'channel-sheet-error';
    error.setAttribute('aria-live', 'polite');
    built.body.appendChild(error);

    var cancel = document.createElement('button');
    cancel.type = 'button';
    cancel.className = 'nr-btn nr-btn-secondary';
    cancel.textContent = 'Cancel';
    cancel.addEventListener('click', function() { built.dismiss(); });
    var prepare = document.createElement('button');
    prepare.type = 'button';
    prepare.className = 'nr-btn nr-btn-primary';
    prepare.textContent = 'Prepare email';
    prepare.addEventListener('click', function() {
        var lines = [
            'I would like to report channel content in Ratspeak.',
            '',
            'Hub: ' + hubName,
            'Hub address: ' + (hubAddress || 'Unavailable'),
            'Channel: ' + (roomName || 'Unavailable'),
            'Reported person: ' + nickname,
            'Identity hash: ' + (identityHash || 'Unavailable'),
            'LXMF address: ' + (lxmfAddress || 'Unavailable'),
            'Event ID: ' + (messageId || 'Unavailable'),
            'Observed at: ' + (timestampMs ? new Date(timestampMs).toISOString() : 'Unavailable')
        ];
        if (messageText) lines.push('', 'Message:', messageText.slice(0, 2000));
        lines.push('', 'Additional context:', '');
        var body = lines.join('\n');
        prepare.disabled = true;
        RS.openSupportEmail('Ratspeak channel report: ' + (roomName || hubName), body).then(function(opened) {
            if (opened) {
                built.dismiss();
                return;
            }
            return RS.copyText(body).then(function() {
                error.textContent = 'No email app was available. The report was copied; send it to mail@ratspeak.org.';
                if (RS.legal && typeof RS.legal.open === 'function') RS.legal.open('support');
            });
        }).catch(function() {
            error.textContent = 'Could not prepare the report. Email mail@ratspeak.org or use the Support page.';
        }).then(function() {
            prepare.disabled = false;
        });
    });
    built.footer.appendChild(cancel);
    built.footer.appendChild(prepare);
    _channelsPresentSheet(built, prepare);
}

function _channelsAppendMemberDetail(room, member, container, options) {
    options = options || {};
    var details = _channelsMemberDetails(member);
    var channelName = _channelsMemberName(member);
    var avatarSize = Number(options.avatarSize) || 52;
    var closeDetail = typeof options.close === 'function'
        ? options.close
        : channelsCloseMemberPane;

    var hero = document.createElement('div');
    hero.className = 'channel-member-detail-hero';
    var avatar = document.createElement('div');
    avatar.className = 'channel-member-detail-avatar';
    _channelsPopulateIdentityAvatar(
        avatar,
        details.lxmfAddress || '',
        avatarSize,
        channelName
    );
    var heroCopy = document.createElement('div');
    heroCopy.className = 'channel-member-detail-hero-copy';
    var name = document.createElement('strong');
    name.textContent = channelName;
    var presence = document.createElement('span');
    presence.textContent = (member._seen
        ? 'Seen in '
        : (member._continuity ? 'Recently visible in ' : 'Visible in ')) +
        _channelsRoomDisplayName(room.name);
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
    container.appendChild(hero);

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
    container.appendChild(fields);

    if (!details.identityHash || !details.lxmfAddress) {
        var hint = document.createElement('p');
        hint.className = 'channel-member-detail-note';
        hint.textContent = !details.identityHash
            ? 'This hub supplied only a channel nickname, so an LXMF address and identity avatar are unavailable.'
            : 'No known LXMF address for this identity yet.';
        container.appendChild(hint);
    }

    var hasActions = false;
    if (!member.is_self) {
        hasActions = true;
        var actions = document.createElement('div');
        actions.className = 'channel-member-detail-actions entity-action-grid';
        var mention = document.createElement('button');
        mention.type = 'button';
        mention.className = 'nr-btn entity-action-btn';
        mention.disabled = !_channelsCanCompose();
        mention.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="4"></circle><path d="M16 8v5a3 3 0 0 0 6 0v-1a10 10 0 1 0-3.9 7.9"></path></svg><span>Mention</span>';
        mention.addEventListener('click', function() {
            if (_channelsInsertMemberMention(member)) closeDetail();
        });
        actions.appendChild(mention);
        if (details.lxmfAddress && typeof openConversationWith === 'function') {
            var message = document.createElement('button');
            message.type = 'button';
            message.className = 'nr-btn entity-action-btn';
            message.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"></path></svg><span>Message</span>';
            message.addEventListener('click', function() {
                closeDetail();
                openConversationWith(details.lxmfAddress);
            });
            actions.appendChild(message);
        }
        var report = document.createElement('button');
        report.type = 'button';
        report.className = 'nr-btn entity-action-btn';
        report.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M5 21V4"/><path d="M5 4h11l-1 4 1 4H5"/></svg><span>Report</span>';
        report.addEventListener('click', function() {
            closeDetail();
            setTimeout(function() {
                _channelsOpenReportSheet({
                    nickname: channelName,
                    identityHash: details.identityHash,
                    lxmfAddress: details.lxmfAddress,
                    room: room.name
                });
            }, 220);
        });
        actions.appendChild(report);
        if (details.lxmfAddress) {
            var block = document.createElement('button');
            block.type = 'button';
            block.className = 'danger-btn entity-action-btn';
            block.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="4.93" y1="4.93" x2="19.07" y2="19.07"/></svg><span>Block</span>';
            block.addEventListener('click', function() {
                rsConfirmWithCheckbox({
                    message: 'Block "' + channelName + '"? Their channel messages and member entry will be hidden.',
                    danger: true,
                    confirmText: 'Block',
                    checkboxLabel: 'Also block at the network layer (drop their packets entirely)',
                    checkboxHelp: 'This affects all identities on this device and may hide more than channel activity.',
                    defaultChecked: false
                }).then(function(result) {
                    if (!result.confirmed) return;
                    return RS.invokeOrToast('block_contact', {
                        args: {
                            hash: details.lxmfAddress,
                            escalate_to_blackhole: result.checked
                        }
                    }, 'Could not block channel member').then(function() {
                        _channelsBlockedAddresses[details.lxmfAddress.toLowerCase()] = true;
                        closeDetail();
                        _channelsRenderAfterSafetyChange();
                    });
                }).catch(function() {});
            });
            actions.appendChild(block);
        }
        (options.actionsContainer || container).appendChild(actions);
    }
    return { hasActions: hasActions };
}

function _channelsFocusMemberRow(memberKey) {
    if (!memberKey) return;
    requestAnimationFrame(function() {
        var rows = document.querySelectorAll('.channel-member-row');
        for (var i = 0; i < rows.length; i++) {
            if (rows[i].dataset.memberKey === memberKey) {
                rows[i].focus();
                break;
            }
        }
    });
}

function _channelsOpenMemberDetailSheet(room, member, memberKey) {
    if (typeof _rsBuildSheet !== 'function') return;
    if (_channelsMemberDetailDismiss) _channelsMemberDetailDismiss();
    _channelsMemberReturnFocusKey = memberKey;
    var built = _rsBuildSheet({ title: 'Member details' }, function() {
        if (_channelsMemberDetailDismiss === built.dismiss) {
            _channelsMemberDetailDismiss = null;
        }
        _channelsFocusMemberRow(memberKey);
    });
    built.sheet.classList.add('channel-member-profile-sheet');
    built.sheet.setAttribute('aria-label', 'Member details for ' + _channelsMemberName(member));
    built.body.classList.add('channel-member-profile-body');
    var result = _channelsAppendMemberDetail(room, member, built.body, {
        avatarSize: 64,
        actionsContainer: built.footer,
        close: built.dismiss
    });
    built.footer.hidden = !result.hasActions;
    _channelsMemberDetailDismiss = built.dismiss;
    _channelsPresentSheet(built);
}

function _channelsRenderMemberDetail(room, member, list, info) {
    var pane = _channelsEl('channel-members-pane');
    var back = _channelsEl('channel-members-back');
    if (pane) pane.classList.add('showing-detail');
    if (back) back.hidden = false;
    if (info) {
        info.hidden = true;
        info.classList.remove('open');
        info.setAttribute('aria-expanded', 'false');
    }
    list.classList.add('showing-detail');
    _channelsAppendMemberDetail(room, member, list, {
        close: channelsCloseMemberPane
    });
}

function _channelsShowMemberList() {
    var focusKey = _channelsMemberReturnFocusKey;
    _channelsSelectedMemberKey = null;
    _channelsMemberReturnFocusKey = null;
    _channelsRenderMembers(channelsActiveRoom ? _channelsRoomByName(channelsActiveRoom) : null);
    _channelsFocusMemberRow(focusKey);
}

function _channelsBuildMemberRow(room, member) {
    var memberKey = _channelsMemberKey(member);
    var nameText = _channelsMemberListName(member);
    var row = document.createElement('button');
    row.type = 'button';
    row.className = 'channel-member-row';
    if (member._continuity) row.classList.add('reconfirming');
    else if (member._seen) row.classList.add('seen');
    row.dataset.memberKey = memberKey;
    var stateLabel = member._continuity
        ? ', recently visible'
        : (member._seen ? ', seen here before' : '');
    row.setAttribute('aria-label', 'View details for ' + nameText + stateLabel);
    var avatar = document.createElement('span');
    avatar.className = 'channel-member-avatar';
    _channelsPopulateIdentityAvatar(
        avatar,
        _channelsIdentityAvatarSeed(member.identity_hash, member.lxmf_hash, !!member.is_self),
        40,
        nameText
    );
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
    row.appendChild(avatar);
    row.appendChild(copy);
    row.appendChild(disclosure);
    row.addEventListener('click', function() {
        if (_channelsCompact()) {
            _channelsOpenMemberDetailSheet(room, member, memberKey);
            return;
        }
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
    return row;
}

function _channelsAppendMemberGroup(list, label, members, room) {
    if (!members.length) return;
    var heading = document.createElement('div');
    heading.className = 'channel-member-group-label';
    heading.textContent = label;
    list.appendChild(heading);
    members.forEach(function(member) {
        list.appendChild(_channelsBuildMemberRow(room, member));
    });
}

function _channelsRenderMembers(room) {
    var list = _channelsEl('channel-members-list');
    var info = _channelsEl('channel-members-info');
    var pane = _channelsEl('channel-members-pane');
    var back = _channelsEl('channel-members-back');
    if (!list) return;
    list.textContent = '';
    list.classList.remove('showing-detail');
    if (pane) pane.classList.remove('showing-detail');
    if (back) back.hidden = true;
    if (info) {
        info.hidden = true;
        info.classList.remove('open');
        info.setAttribute('aria-expanded', 'false');
    }
    var model = _channelsMemberRosterModel(room);
    var members = model.visible.concat(model.continuity, model.seen);
    var selectedMember = _channelsSelectedMemberKey
        ? _channelsMemberByKey(members, _channelsSelectedMemberKey)
        : null;
    if (_channelsSelectedMemberKey && !selectedMember) {
        _channelsSelectedMemberKey = null;
        _channelsMemberReturnFocusKey = null;
    }
    if (room && selectedMember) {
        _channelsRenderMemberDetail(room, selectedMember, list, info);
        return;
    }

    _channelsSetText('channel-members-label', 'People here');
    _channelsSetText('channel-members-count', model.visible.length + ' visible');
    if (room && room.phase !== 'joined') {
        var waiting = document.createElement('div');
        waiting.className = 'channel-members-empty';
        waiting.textContent = 'Waiting for channel membership.';
        list.appendChild(waiting);
        return;
    }
    if (info) info.hidden = !room;
    if (!members.length) {
        var empty = document.createElement('div');
        empty.className = 'channel-members-empty';
        empty.textContent = room ? 'No member details have been supplied by this hub yet.' : 'Join a channel to see the people the hub reports.';
        list.appendChild(empty);
        return;
    }
    model.visible.forEach(function(member) {
        list.appendChild(_channelsBuildMemberRow(room, member));
    });
    _channelsAppendMemberGroup(list, 'Recently visible', model.continuity, room);
    _channelsAppendMemberGroup(list, 'Seen here', model.seen, room);
    if (model.omitted > 0) {
        var omitted = document.createElement('div');
        omitted.className = 'channel-member-history-omitted';
        omitted.textContent = '+' + model.omitted + ' more in local history';
        list.appendChild(omitted);
    }
}

function _channelsRefreshMemberNamesFromPeers() {
    if (!_channelsViewVisible()) return;
    var room = channelsActiveRoom ? _channelsRoomByName(channelsActiveRoom) : null;
    if (room) _channelsRenderMembers(room);
}

function _channelsUpdateComposer() {
    var input = _channelsEl('channel-message-input');
    var count = _channelsEl('channel-char-count');
    var send = _channelsEl('channel-send-btn');
    if (!input || !count || !send) return;
    var limit = _channelsMessageLimit();
    var used = _channelsUtf8Length(_channelsMessageBody(input.value));
    count.textContent = used >= Math.floor(limit * 0.75) ? used + '/' + limit : '';
    count.hidden = !count.textContent;
    count.classList.toggle('near-limit', used >= Math.floor(limit * 0.75) && used <= limit);
    count.classList.toggle('over-limit', used > limit);
    var room = channelsActiveRoom ? _channelsRoomByName(channelsActiveRoom) : null;
    send.disabled = _channelsSendPending || !room || room.phase !== 'joined' || channelsSnapshot.phase !== 'active' || !input.value.trim() || used > limit;
    input.placeholder = channelsSnapshot.phase === 'stale' ? 'Connection recovering\u2026' : 'Message...';
    input.disabled = !room || room.phase !== 'joined' || channelsSnapshot.phase !== 'active';
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
    if (_channelsMemberDetailDismiss) {
        var dismissDetail = _channelsMemberDetailDismiss;
        _channelsMemberDetailDismiss = null;
        dismissDetail();
    }
    _channelsSelectedMemberKey = null;
    _channelsMemberReturnFocusKey = null;
    _channelsRenderMembers(channelsActiveRoom ? _channelsRoomByName(channelsActiveRoom) : null);
    var layout = _channelsEl('channels-layout');
    if (layout) layout.classList.remove('members-open');
}

function channelsHandleMemberPaneBack() {
    var layout = _channelsEl('channels-layout');
    if (!layout || !layout.classList.contains('members-open')) return false;
    if (_channelsSelectedMemberKey) {
        _channelsShowMemberList();
    } else {
        channelsCloseMemberPane();
    }
    return true;
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
            title: target.room ? 'Share channel' : 'Share hub'
        }, function() {});
        built.sheet.classList.add('channel-share-sheet');
        var copy = document.createElement('p');
        copy.className = 'channel-sheet-copy';
        copy.textContent = target.room
            ? 'Invite someone to ' + _channelsRoomDisplayName(target.room) + '.'
            : 'Invite someone to connect to this hub.';
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
                        ok ? 'toast-success' : 'toast-error',
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
                        method === 'share' ? 'QR code shared' : 'QR image saved',
                        'toast-success',
                        2400
                    );
                }
            }).catch(function(error) {
                if (typeof showToast === 'function') {
                    showToast(
                        (error && error.message) || 'Could not share channel QR',
                        'toast-error',
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
        note.textContent = 'The link contains no channel key and never joins automatically.';
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
                'toast-error',
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

    var built = _rsBuildSheet({
        title: target.room ? 'Shared channel' : 'Shared hub'
    }, function() {});
    built.sheet.classList.add('channel-share-preview-sheet');
    var copy = document.createElement('p');
    copy.className = 'channel-sheet-copy';
    copy.textContent = target.room
        ? _channelsRoomDisplayName(target.room) + ' on ' + _channelsTimelineHubName(hub)
        : _channelsTimelineHubName(hub);
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
    trust.textContent = 'This link identifies a destination; it does not prove who runs it or contain a channel key.';
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
            showToast('QR scanning is not available in this build', 'toast-warning', 2800);
        }
        return;
    }
    RS.qr.openScanner({
        title: 'Scan Ratspeak QR',
        checkingText: 'Checking link\u2026',
        previewCommand: 'api_preview_channel_share',
        invalidText: 'That QR is not a valid Ratspeak link.',
        invalidImageText: 'That image does not contain a valid Ratspeak QR.',
        emptyImageText: 'No Ratspeak QR found in that image.',
        onPreview: function(_body, _payload, target, closeAll) {
            closeAll();
            setTimeout(function() { _channelsPresentSharedTarget(target); }, 220);
        }
    });
}

function _channelsBuildLinkMethod(titleText, detailText, iconMarkup) {
    var button = document.createElement('button');
    button.type = 'button';
    button.className = 'channel-link-method';
    var icon = document.createElement('span');
    icon.className = 'channel-link-method-icon';
    icon.innerHTML = iconMarkup;
    var copy = document.createElement('span');
    copy.className = 'channel-link-method-copy';
    var title = document.createElement('strong');
    title.textContent = titleText;
    var detail = document.createElement('span');
    detail.textContent = detailText;
    copy.appendChild(title);
    copy.appendChild(detail);
    button.appendChild(icon);
    button.appendChild(copy);
    return button;
}

function channelsOpenSharedChannel(payload) {
    if (typeof _rsBuildSheet !== 'function') return;
    var built = _rsBuildSheet({ title: 'Open link or QR' }, function() {});
    built.sheet.classList.add('channel-open-link-sheet');
    var copy = document.createElement('p');
    copy.className = 'channel-sheet-copy';
    copy.textContent = 'Open an invitation without connecting automatically.';
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

    var tools = document.createElement('div');
    tools.className = 'channel-link-methods';
    var scan = _channelsBuildLinkMethod(
        'Scan QR',
        'Use the camera or choose an image',
        '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="M4 7V4h3M17 4h3v3M20 17v3h-3M7 20H4v-3"/><rect x="8" y="8" width="3" height="3"/><rect x="14" y="8" width="2" height="2"/><rect x="8" y="14" width="2" height="2"/><path d="M14 14h2v2h2v2h-4z"/></svg>'
    );
    scan.hidden = !RS.qr || typeof RS.qr.openScanner !== 'function';
    scan.addEventListener('click', function() {
        built.dismiss();
        setTimeout(channelsScanSharedChannel, 220);
    });
    tools.appendChild(scan);
    var paste = _channelsBuildLinkMethod(
        'Paste from clipboard',
        'Review a copied Ratspeak link',
        _channelsActionIcon('copy')
    );
    paste.hidden = !navigator.clipboard || typeof navigator.clipboard.readText !== 'function';
    paste.addEventListener('click', function() {
        navigator.clipboard.readText().then(function(text) {
            input.value = text || '';
            manual.open = true;
            preview();
        }).catch(function() {
            error.textContent = 'Clipboard access was unavailable. Paste the link into the field.';
            manual.open = true;
            input.focus();
        });
    });
    tools.appendChild(paste);
    built.body.appendChild(tools);

    var manual = document.createElement('details');
    manual.className = 'channel-private-access channel-link-manual';
    var manualSummary = document.createElement('summary');
    manualSummary.textContent = 'Enter link manually';
    manual.appendChild(manualSummary);
    var manualBody = document.createElement('div');
    manualBody.className = 'channel-private-access-body';
    manualBody.appendChild(_channelsSheetField('Ratspeak link', input));
    manual.appendChild(manualBody);
    manual.open = !!payload || (scan.hidden && paste.hidden);
    built.body.appendChild(manual);

    var error = document.createElement('div');
    error.className = 'channel-sheet-error';
    error.setAttribute('aria-live', 'polite');
    built.body.appendChild(error);
    function preview() {
        var raw = input.value.trim();
        if (!raw) {
            error.textContent = 'Paste a Ratspeak link or scan its QR.';
            manual.open = true;
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
                'That is not a valid Ratspeak link.';
            manual.open = true;
            open.disabled = false;
            open.textContent = 'Review';
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
    open.textContent = 'Review';
    open.addEventListener('click', preview);
    built.footer.appendChild(cancel);
    built.footer.appendChild(open);
    _channelsPresentSheet(built, scan.hidden ? (paste.hidden ? input : paste) : scan);
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
    var built = _rsBuildSheet({
        title: _channelsIsConnected() || _channelsIsConnecting()
            ? 'Switch hub'
            : 'Choose a hub'
    }, function() {
        if (_channelsHubSwitcherDismiss === built.dismiss) {
            _channelsHubSwitcherDismiss = null;
        }
    });
    _channelsHubSwitcherDismiss = built.dismiss;
    built.sheet.classList.add('channel-hub-switcher-sheet');

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
            showToast('Channels changed. Choose a hub again.', 'toast-warning', 2800);
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
            else channelsOpenConnectSheet(Object.assign({}, hub, {
                return_to_switcher: true
            }));
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
                if (nextRefresh && RS.ui && typeof RS.ui.focusAfterUpdate === 'function') {
                    RS.ui.focusAfterUpdate(nextRefresh);
                }
            } else if (built.sheet.isConnected) {
                retireStaleSwitcher();
            }
        }, function() {
            refreshInFlight = false;
            list.removeAttribute('aria-busy');
            if (built.sheet.isConnected && contextIsCurrent()) {
                renderList();
                var nextRefresh = list.querySelector('[data-channel-hub-refresh]');
                if (nextRefresh && RS.ui && typeof RS.ui.focusAfterUpdate === 'function') {
                    RS.ui.focusAfterUpdate(nextRefresh);
                }
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
            appendHubSection('Current', [model.current], {
                current: true,
                status: currentStatus.label,
                statusTone: currentStatus.tone
            });
        }
        appendHubSection('Nearby', model.nearby, {
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
        appendHubSection('Saved', model.saved, {
            disabled: blocked
        });
        if (!model.current && !model.nearby.length && !model.saved.length) {
            list.appendChild(_channelsDirectoryStatus(
                'No channel hubs yet',
                'Enter a trusted hub address to begin'
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

    var note = document.createElement('p');
    note.className = 'channel-hub-switcher-note';
    note.textContent = 'One hub can be live at a time. Switching ends live channel sessions; history stays on this device.';
    built.body.appendChild(note);

    var add = document.createElement('button');
    add.type = 'button';
    add.className = 'nr-btn nr-btn-primary';
    add.textContent = 'Enter address';
    add.addEventListener('click', function() {
        built.dismiss();
        setTimeout(function() {
            channelsOpenConnectSheet({ return_to_switcher: true });
        }, 220);
    });
    var done = document.createElement('button');
    done.type = 'button';
    done.className = 'nr-btn nr-btn-secondary';
    done.textContent = 'Close';
    done.addEventListener('click', built.dismiss);
    built.footer.appendChild(done);
    built.footer.appendChild(add);

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
    if (!prefill.public_consent_checked) {
        _channelsEnsurePublicConsent().then(function(accepted) {
            if (!accepted) return;
            channelsOpenConnectSheet(Object.assign({}, prefill, {
                public_consent_checked: true
            }));
        }).catch(function(err) {
            if (typeof showToast === 'function') {
                showToast((err && err.message) || 'Could not load channel safety settings.', 'toast-error', 5000);
            }
        });
        return;
    }
    var selectedHash = String(prefill.destination_hash || '').trim().toLowerCase();
    var selectedLabel = prefill.label || prefill.announced_name || '';
    var sharedDestination = prefill.shared_room ? selectedHash : '';
    var sharedRoom = prefill.shared_room || '';
    var manualEntry = !selectedHash;
    var initialMode = selectedHash ? _channelsHubConnectMode(selectedHash) : null;
    if (initialMode && initialMode.kind === 'current' && !sharedRoom) {
        channelsOpenHubOptions();
        return;
    }

    var selectedHub = selectedHash ? _channelsHubByDestination(selectedHash) : null;
    var defaultNick = prefill.nickname ||
        (_channelsSavedHub(selectedHash) || {}).nickname ||
        channelsSnapshot.nickname || _channelsDefaultNickname();
    var built = _rsBuildSheet({ title: manualEntry ? 'Connect to a hub' : 'Review hub' }, function() {});
    built.sheet.classList.add('channel-connect-sheet');
    var titleElement = built.sheet.querySelector('.bottom-sheet-title');

    if (sharedRoom) {
        var shareContext = document.createElement('div');
        shareContext.className = 'channel-connection-context';
        var shareLabel = document.createElement('span');
        shareLabel.textContent = 'Shared channel';
        var shareName = document.createElement('strong');
        shareName.textContent = _channelsRoomDisplayName(sharedRoom);
        shareContext.appendChild(shareLabel);
        shareContext.appendChild(shareName);
        built.body.appendChild(shareContext);
    }

    if (selectedHub) {
        var summary = document.createElement('div');
        summary.className = 'channel-connection-summary';
        summary.appendChild(_channelsBuildHubMark(selectedHub));
        var summaryCopy = document.createElement('span');
        summaryCopy.className = 'channel-connection-summary-copy';
        var summaryTitle = document.createElement('strong');
        summaryTitle.textContent = selectedLabel || _channelsTimelineHubName(selectedHub);
        var summaryMeta = document.createElement('span');
        var summaryParts = [_channelsShortHash(selectedHash)];
        var distance = _channelsHubDistance(selectedHub);
        if (distance) summaryParts.push(distance);
        summaryMeta.textContent = summaryParts.join(' \u00b7 ');
        summaryCopy.appendChild(summaryTitle);
        summaryCopy.appendChild(summaryMeta);
        summary.appendChild(summaryCopy);
        built.body.appendChild(summary);
    }

    var destinationInput = document.createElement('input');
    destinationInput.type = 'text';
    destinationInput.className = 'nr-input-sm mono';
    destinationInput.placeholder = '32-character hub address';
    destinationInput.autocapitalize = 'none';
    destinationInput.autocomplete = 'off';
    destinationInput.spellcheck = false;
    destinationInput.maxLength = 32;
    destinationInput.value = selectedHash;
    var destinationField = _channelsSheetField('Hub address', destinationInput);
    destinationField.hidden = !manualEntry;
    built.body.appendChild(destinationField);

    var nicknameInput = document.createElement('input');
    nicknameInput.type = 'text';
    nicknameInput.className = 'nr-input-sm';
    nicknameInput.placeholder = 'Nickname for this session';
    nicknameInput.maxLength = 32;
    nicknameInput.value = defaultNick;
    var nicknameField = _channelsSheetField('Nickname', nicknameInput);
    nicknameField.hidden = !manualEntry;

    var identityRow = document.createElement('div');
    identityRow.className = 'channel-connection-identity';
    identityRow.hidden = manualEntry;
    var identityCopy = document.createElement('span');
    var identityLabel = document.createElement('span');
    identityLabel.textContent = 'Joining as';
    var identityName = document.createElement('strong');
    identityName.textContent = defaultNick;
    identityCopy.appendChild(identityLabel);
    identityCopy.appendChild(identityName);
    var changeNickname = document.createElement('button');
    changeNickname.type = 'button';
    changeNickname.className = 'channel-connection-change';
    changeNickname.textContent = 'Change';
    changeNickname.addEventListener('click', function() {
        identityRow.hidden = true;
        nicknameField.hidden = false;
        nicknameInput.focus();
        nicknameInput.select();
    });
    identityRow.appendChild(identityCopy);
    identityRow.appendChild(changeNickname);
    built.body.appendChild(identityRow);
    built.body.appendChild(nicknameField);

    var switchImpact = document.createElement('div');
    switchImpact.className = 'channel-hub-switch-impact';
    switchImpact.hidden = true;
    built.body.appendChild(switchImpact);

    var error = document.createElement('div');
    error.className = 'channel-sheet-error';
    error.setAttribute('aria-live', 'polite');
    built.body.appendChild(error);

    var cancel = document.createElement('button');
    cancel.type = 'button';
    cancel.className = 'nr-btn nr-btn-secondary';
    cancel.textContent = prefill.return_to_switcher ? 'Back' : 'Cancel';
    cancel.addEventListener('click', function() {
        built.dismiss();
        if (prefill.return_to_switcher) {
            setTimeout(channelsOpenHubSwitcher, 220);
        }
    });
    var connect = document.createElement('button');
    connect.type = 'button';
    connect.className = 'nr-btn nr-btn-primary';
    var connectBusy = false;

    function destinationName(destination) {
        var hub = _channelsHubByDestination(destination);
        return selectedLabel || _channelsTimelineHubName(hub);
    }

    function updateConnectionChoice() {
        var destination = destinationInput.value.trim().toLowerCase();
        var mode = _channelsHubConnectMode(destination);
        var nextName = destination && /^[0-9a-f]{32}$/.test(destination)
            ? destinationName(destination)
            : 'this hub';
        switchImpact.hidden = true;

        if (mode.kind === 'switch') {
            if (titleElement) titleElement.textContent = 'Switch to ' + nextName + '?';
            connect.textContent = connectBusy
                ? 'Switching\u2026'
                : (sharedRoom ? 'Switch and review' : 'Switch');
            connect.disabled = connectBusy;
        } else if (mode.kind === 'current') {
            if (titleElement) titleElement.textContent = nextName;
            connect.textContent = 'View hub';
            connect.disabled = false;
        } else if (mode.kind === 'recovering') {
            if (titleElement) titleElement.textContent = 'Reconnecting to ' + nextName;
            switchImpact.hidden = false;
            switchImpact.textContent = 'Open hub details to stop or review this connection.';
            connect.textContent = 'View hub';
            connect.disabled = false;
        } else if (mode.kind === 'pending') {
            if (titleElement) titleElement.textContent = 'Connection in progress';
            switchImpact.hidden = false;
            switchImpact.textContent = 'Finish or cancel the current connection before choosing another hub.';
            connect.textContent = 'Connection in progress';
            connect.disabled = true;
        } else {
            if (titleElement) {
                titleElement.textContent = manualEntry
                    ? 'Connect to a hub'
                    : 'Connect to ' + nextName + '?';
            }
            connect.textContent = connectBusy
                ? 'Connecting\u2026'
                : (sharedRoom ? 'Connect and review' : 'Connect');
            connect.disabled = connectBusy;
        }
    }

    destinationInput.addEventListener('input', function() {
        var entered = destinationInput.value.trim().toLowerCase();
        if (entered !== selectedHash) selectedLabel = '';
        if (entered !== sharedDestination) sharedRoom = '';
        error.textContent = '';
        updateConnectionChoice();
    });

    connect.addEventListener('click', function() {
        var destination = destinationInput.value.trim().toLowerCase();
        var nickname = nicknameInput.value.trim();
        var connectMode = _channelsHubConnectMode(destination);
        if (connectMode.kind === 'current' || connectMode.kind === 'recovering') {
            built.dismiss();
            setTimeout(function() { channelsOpenHubOptions(); }, 220);
            return;
        }
        if (!/^[0-9a-f]{32}$/.test(destination)) {
            error.textContent = 'Enter a 32-character hexadecimal hub address.';
            destinationField.hidden = false;
            destinationInput.focus();
            return;
        }
        if (!nickname) {
            error.textContent = 'Choose a nickname for this session.';
            identityRow.hidden = true;
            nicknameField.hidden = false;
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
    _channelsPresentSheet(built, manualEntry ? destinationInput : connect);
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
            handleSelector: '.bottom-sheet-handle',
            blockIfScrolled: true,
            skipIf: function(event) {
                return !!event.target.closest('button, input, textarea, select');
            },
            parallaxOverlay: built.overlay,
            onCommit: function() { built.dismiss(); }
        });
    }
    built.present();
    if (initialFocus) setTimeout(function() {
        if (RS.ui && typeof RS.ui.focusAfterUpdate === 'function') RS.ui.focusAfterUpdate(initialFocus);
        else if (!_channelsCompact()) initialFocus.focus();
    }, 250);
}

function channelsOpenJoinSheet(prefillRoom, options) {
    if (!_channelsIsConnected() || typeof _rsBuildSheet !== 'function') return;
    options = options || {};
    if (!options.public_consent_checked) {
        _channelsEnsurePublicConsent().then(function(accepted) {
            if (accepted) channelsOpenJoinSheet(prefillRoom, { public_consent_checked: true });
        }).catch(function(err) {
            if (typeof showToast === 'function') {
                showToast((err && err.message) || 'Could not load channel safety settings.', 'toast-error', 5000);
            }
        });
        return;
    }
    var normalizedPrefill = String(prefillRoom || '').trim().toLowerCase();
    var directoryRooms = channelsSnapshot.directory &&
        Array.isArray(channelsSnapshot.directory.rooms)
        ? channelsSnapshot.directory.rooms
        : [];
    var directoryRoom = directoryRooms.find(function(room) {
        return room && String(room.name || '').toLowerCase() === normalizedPrefill;
    }) || null;
    var built = _rsBuildSheet({
        title: normalizedPrefill ? 'Join ' + _channelsRoomDisplayName(normalizedPrefill) + '?' : 'Join a channel'
    }, function() {});
    built.sheet.classList.add('channel-join-sheet');

    if (normalizedPrefill) {
        var preview = document.createElement('div');
        preview.className = 'channel-join-preview';
        var previewName = document.createElement('strong');
        previewName.textContent = _channelsRoomDisplayName(normalizedPrefill);
        var previewTopic = document.createElement('span');
        previewTopic.textContent = directoryRoom && directoryRoom.topic
            ? directoryRoom.topic
            : 'Channel on ' + _channelsHubName(channelsSnapshot.hub);
        var previewMeta = document.createElement('small');
        previewMeta.textContent = _channelsHubName(channelsSnapshot.hub);
        preview.appendChild(previewName);
        preview.appendChild(previewTopic);
        preview.appendChild(previewMeta);
        built.body.appendChild(preview);
    }

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
    roomInput.value = normalizedPrefill;
    var roomField = _channelsSheetField('Channel name', roomInput);
    roomField.hidden = !!normalizedPrefill;
    built.body.appendChild(roomField);

    var privateAccess = document.createElement('details');
    privateAccess.className = 'channel-private-access';
    var privateSummary = document.createElement('summary');
    privateSummary.textContent = 'Private channel?';
    privateAccess.appendChild(privateSummary);
    var privateBody = document.createElement('div');
    privateBody.className = 'channel-private-access-body';
    var keyInput = document.createElement('input');
    keyInput.type = 'password';
    keyInput.className = 'nr-input-sm';
    keyInput.placeholder = 'Access key';
    keyInput.autocomplete = 'off';
    keyInput.maxLength = 1024;
    privateBody.appendChild(_channelsSheetField('Access key', keyInput));
    var rememberRow = document.createElement('label');
    rememberRow.className = 'rs-dialog-checkbox-row channel-key-remember';
    rememberRow.hidden = true;
    var rememberKey = document.createElement('input');
    rememberKey.type = 'checkbox';
    rememberKey.checked = true;
    rememberKey.disabled = true;
    var rememberLabel = document.createElement('span');
    rememberLabel.textContent = 'Remember for reconnect';
    rememberRow.appendChild(rememberKey);
    rememberRow.appendChild(rememberLabel);
    privateBody.appendChild(rememberRow);
    var note = document.createElement('p');
    note.className = 'channel-private-access-note';
    note.hidden = true;
    privateBody.appendChild(note);
    privateAccess.appendChild(privateBody);
    built.body.appendChild(privateAccess);
    function updateKeyPolicy() {
        rememberKey.disabled = !keyInput.value;
        var savedRoom = _channelsDurableRoom(roomInput.value.trim().toLowerCase());
        if (!keyInput.value && savedRoom && savedRoom.has_stored_join_key) {
            privateAccess.open = true;
            rememberRow.hidden = true;
            note.hidden = false;
            note.textContent = 'A saved access key is ready. Leave this blank to use it.';
        } else if (keyInput.value) {
            rememberRow.hidden = false;
            note.hidden = false;
            note.textContent = rememberKey.checked
                ? 'Ratspeak saves only identity-sealed ciphertext, and only after the hub confirms membership.'
                : 'This key will be used for this session only.';
        } else {
            rememberRow.hidden = true;
            note.hidden = true;
            note.textContent = '';
        }
    }
    keyInput.addEventListener('input', updateKeyPolicy);
    roomInput.addEventListener('input', updateKeyPolicy);
    rememberKey.addEventListener('change', updateKeyPolicy);
    if (directoryRoom && String(directoryRoom.modes || '').indexOf('k') !== -1) {
        privateAccess.open = true;
    }
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
            if (/key|invite|private/i.test(error.textContent)) privateAccess.open = true;
            join.disabled = false;
            join.textContent = 'Join';
        });
    });
    built.footer.appendChild(cancel);
    built.footer.appendChild(join);
    _channelsPresentSheet(built, normalizedPrefill ? join : roomInput);
}

function _channelsHubProfileDetail(labelText, value, mono) {
    var row = _channelsRoomDetail(labelText, value);
    row.classList.add('channel-hub-profile-detail');
    if (mono) row.querySelector('strong').classList.add('mono');
    return row;
}

function _channelsHubProfileSection(titleText, copyText) {
    var section = document.createElement('section');
    section.className = 'channel-hub-profile-section';
    var title = document.createElement('h3');
    title.textContent = titleText;
    section.appendChild(title);
    if (copyText) {
        var copy = document.createElement('p');
        copy.textContent = copyText;
        section.appendChild(copy);
    }
    return section;
}

function _channelsHubCapability(labelText, description, enabled) {
    var capability = document.createElement('div');
    capability.className = 'channel-hub-profile-capability';
    capability.dataset.enabled = enabled ? 'true' : 'false';
    var mark = document.createElement('span');
    mark.className = 'channel-hub-profile-capability-mark';
    mark.setAttribute('aria-hidden', 'true');
    mark.textContent = enabled ? '\u2713' : '\u2014';
    var copy = document.createElement('span');
    copy.className = 'channel-hub-profile-capability-copy';
    var label = document.createElement('strong');
    label.textContent = labelText;
    var detail = document.createElement('span');
    detail.textContent = enabled ? description : 'Not advertised by this hub';
    copy.appendChild(label);
    copy.appendChild(detail);
    capability.appendChild(mark);
    capability.appendChild(copy);
    return capability;
}

function _channelsActionIcon(kind) {
    if (kind === 'share') {
        return '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="18" cy="5" r="3"/><circle cx="6" cy="12" r="3"/><circle cx="18" cy="19" r="3"/><path d="m8.6 10.5 6.8-4M8.6 13.5l6.8 4"/></svg>';
    }
    if (kind === 'copy') {
        return '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>';
    }
    if (kind === 'leave') {
        return '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M10 17l5-5-5-5"/><path d="M15 12H3"/><path d="M14 3h5a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2h-5"/></svg>';
    }
    return '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><path d="M12 16v-4"/><path d="M12 8h.01"/></svg>';
}

function _channelsDisconnectFromHub(control) {
    channelsPendingShareJoin = null;
    if (control) control.disabled = true;
    return RS.invoke('disconnect_channel_hub').then(function(snapshot) {
        channelsActiveRoom = null;
        channelsHistorySelection = null;
        channelsApplySnapshot(snapshot);
        return snapshot;
    }).catch(function(err) {
        if (control) control.disabled = false;
        if (typeof showToast === 'function') {
            showToast((err && err.message) || 'Could not disconnect', 'toast-error', 3200);
        }
        return null;
    });
}

function channelsOpenHubOptions(eventOrTrigger) {
    if (!channelsSnapshot.hub) return;
    var trigger = eventOrTrigger && eventOrTrigger.currentTarget
        ? eventOrTrigger.currentTarget
        : (eventOrTrigger && eventOrTrigger.nodeType === 1 ? eventOrTrigger : null);
    if (!trigger || !RS.ui || typeof RS.ui.openActionMenu !== 'function') {
        channelsOpenHubDetails();
        return;
    }
    var hub = channelsSnapshot.hub;
    var displayName = _channelsHubName(hub);
    var canceling = _channelsIsConnecting();
    RS.ui.openActionMenu(trigger, [
        {
            label: 'View hub details',
            icon: _channelsActionIcon('info'),
            onSelect: channelsOpenHubDetails
        },
        {
            label: 'Share hub',
            icon: _channelsActionIcon('share'),
            onSelect: function() {
                channelsOpenChannelShare(hub.destination_hash, null);
            }
        },
        {
            label: 'Copy address',
            icon: _channelsActionIcon('copy'),
            onSelect: function() {
                RS.copyText(hub.destination_hash).then(function(ok) {
                    if (typeof showToast === 'function') {
                        showToast(ok ? 'Hub address copied' : 'Could not copy', ok ? 'toast-success' : 'toast-error', 1800);
                    }
                });
            }
        },
        { separator: true },
        {
            label: canceling ? 'Cancel connection' : 'Disconnect',
            icon: _channelsActionIcon('leave'),
            danger: true,
            onSelect: function() {
                var proceed = typeof rsConfirm === 'function'
                    ? rsConfirm({
                        title: canceling ? 'Cancel connection?' : 'Disconnect from ' + displayName + '?',
                        message: canceling
                            ? 'Stop this connection attempt.'
                            : 'Live channel sessions will end. Local history stays on this device.',
                        confirmText: canceling ? 'Cancel connection' : 'Disconnect',
                        danger: true
                    })
                    : Promise.resolve(true);
                proceed.then(function(confirmed) {
                    if (confirmed) _channelsDisconnectFromHub();
                });
            }
        }
    ], { title: displayName, showTitle: false });
}

function channelsOpenHubDetails() {
    if (!channelsSnapshot.hub || typeof _rsBuildSheet !== 'function') return;
    var profile = _channelsHubProfileModel();
    var hub = profile.hub;
    var built = _rsBuildSheet({ title: profile.display_name }, function() {});
    built.sheet.classList.add('channel-hub-profile-sheet');
    built.body.classList.add('channel-hub-profile');

    var hero = document.createElement('header');
    hero.className = 'channel-hub-profile-hero';
    hero.dataset.phase = profile.phase;
    var eyebrow = document.createElement('div');
    eyebrow.className = 'channel-hub-profile-eyebrow';
    var statusDot = document.createElement('span');
    statusDot.className = 'channel-hub-profile-status-dot';
    statusDot.setAttribute('aria-hidden', 'true');
    var status = document.createElement('span');
    status.textContent = profile.authenticated_session
        ? 'Authenticated channel hub'
        : 'Channel hub connection';
    eyebrow.appendChild(statusDot);
    eyebrow.appendChild(status);
    var summary = document.createElement('p');
    var summaryParts = [_channelsPhaseLabel(profile.phase)];
    if (profile.hops != null) {
        summaryParts.push(profile.hops + (profile.hops === 1 ? ' hop' : ' hops'));
    }
    if (profile.hub_version) summaryParts.push('Hub software ' + profile.hub_version);
    summary.textContent = summaryParts.join(' \u00b7 ');
    hero.appendChild(eyebrow);
    hero.appendChild(summary);
    built.body.appendChild(hero);

    if (profile.name_mismatch) {
        var mismatch = document.createElement('div');
        mismatch.className = 'channel-hub-profile-mismatch';
        mismatch.setAttribute('role', 'note');
        var mismatchTitle = document.createElement('strong');
        mismatchTitle.textContent = 'Name differs from the recent announce';
        var mismatchCopy = document.createElement('span');
        mismatchCopy.textContent = 'The announce said \u201c' + profile.announced_name +
            '\u201d; this authenticated WELCOME says \u201c' +
            profile.authenticated_name + '\u201d.';
        mismatch.appendChild(mismatchTitle);
        mismatch.appendChild(mismatchCopy);
        built.body.appendChild(mismatch);
    }

    if (profile.greeting) {
        built.body.appendChild(_channelsBuildHubGreeting(profile.greeting, true));
    } else {
        var noGuidance = document.createElement('div');
        noGuidance.className = 'channel-hub-profile-empty-guidance';
        noGuidance.textContent =
            'This hub did not send welcome or rules guidance for this Link session.';
        built.body.appendChild(noGuidance);
    }

    var technical = document.createElement('details');
    technical.className = 'channel-hub-technical';
    var technicalSummary = document.createElement('summary');
    technicalSummary.textContent = 'Technical details';
    technical.appendChild(technicalSummary);

    var identitySection = _channelsHubProfileSection(
        'Connection identity',
        profile.authenticated_session
            ? 'These values come from the destination and authenticated Link, not from a share label.'
            : 'Ratspeak has no active authenticated WELCOME for this hub right now.'
    );
    var identityDetails = document.createElement('div');
    identityDetails.className = 'channel-room-details channel-hub-profile-details';
    identityDetails.appendChild(_channelsHubProfileDetail(
        'Destination',
        profile.destination_hash || 'Unavailable',
        true
    ));
    identityDetails.appendChild(_channelsHubProfileDetail(
        'Hub identity',
        profile.identity_hash || 'Unavailable',
        true
    ));
    if (profile.authenticated_name) {
        identityDetails.appendChild(_channelsHubProfileDetail(
            'WELCOME name',
            profile.authenticated_name
        ));
    }
    if (profile.announced_name) {
        identityDetails.appendChild(_channelsHubProfileDetail(
            'Recent announce',
            profile.announced_name
        ));
    }
    if (profile.nickname) {
        identityDetails.appendChild(_channelsHubProfileDetail(
            'Your nickname',
            profile.nickname
        ));
    }
    identityDetails.appendChild(_channelsHubProfileDetail(
        'Path',
        profile.hops == null
            ? 'Hop count unavailable'
            : profile.hops + (profile.hops === 1 ? ' hop' : ' hops')
    ));
    if (profile.link_mdu != null) {
        identityDetails.appendChild(_channelsHubProfileDetail(
            'Link MDU',
            profile.link_mdu + ' bytes'
        ));
    }
    if (profile.protocol_version) {
        identityDetails.appendChild(_channelsHubProfileDetail(
            'RRC profile',
            profile.protocol_version
        ));
    }
    identitySection.appendChild(identityDetails);
    technical.appendChild(identitySection);

    var directorySection = _channelsHubProfileSection(
        'Public directory',
        profile.directory.summary
    );
    var directoryNote = document.createElement('p');
    directoryNote.className = 'channel-hub-profile-note';
    directoryNote.textContent =
        'Only public channels disclosed on this Link appear here. Private or secret channels may be intentionally hidden.';
    directorySection.appendChild(directoryNote);
    technical.appendChild(directorySection);

    var capabilitySection = _channelsHubProfileSection(
        'Session capabilities',
        'Advertised by this hub in the authenticated WELCOME.'
    );
    var capabilityGrid = document.createElement('div');
    capabilityGrid.className = 'channel-hub-profile-capabilities';
    capabilityGrid.appendChild(_channelsHubCapability(
        'Action messages',
        'Supports action-style channel posts',
        profile.capabilities.actions
    ));
    capabilityGrid.appendChild(_channelsHubCapability(
        'Direct notices',
        'Can address a hub notice to one session',
        profile.capabilities.direct_notices
    ));
    capabilityGrid.appendChild(_channelsHubCapability(
        'Complete welcome transfer',
        'Can deliver bounded welcome guidance as a Resource',
        profile.capabilities.resource_envelopes
    ));
    capabilityGrid.appendChild(_channelsHubCapability(
        'Invite rejoin grace',
        'May restore a short identity-bound invite after reconnect',
        profile.capabilities.rejoin_grace
    ));
    capabilitySection.appendChild(capabilityGrid);
    technical.appendChild(capabilitySection);

    var limitRows = [];
    if (profile.limits.max_message_body_bytes != null) {
        limitRows.push(['Message body', profile.limits.max_message_body_bytes + ' UTF-8 bytes']);
    }
    if (profile.limits.max_rooms_per_session != null) {
        limitRows.push([
            'Joined channels',
            profile.limits.max_rooms_per_session + ' per session'
        ]);
    }
    if (profile.limits.rate_messages_per_minute != null) {
        limitRows.push([
            'Message rate',
            profile.limits.rate_messages_per_minute + ' per minute'
        ]);
    }
    if (profile.limits.max_nick_bytes != null) {
        limitRows.push(['Nickname', profile.limits.max_nick_bytes + ' UTF-8 bytes']);
    }
    if (profile.limits.max_room_name_bytes != null) {
        limitRows.push([
            'Channel name',
            profile.limits.max_room_name_bytes + ' UTF-8 bytes'
        ]);
    }
    var limitsSection = _channelsHubProfileSection(
        'Hub limits',
        limitRows.length
            ? 'Applied by the hub to this session.'
            : 'This hub did not advertise concrete session limits.'
    );
    if (limitRows.length) {
        var limitDetails = document.createElement('div');
        limitDetails.className = 'channel-room-details channel-hub-profile-details';
        limitRows.forEach(function(row) {
            limitDetails.appendChild(_channelsHubProfileDetail(row[0], row[1]));
        });
        limitsSection.appendChild(limitDetails);
    }
    technical.appendChild(limitsSection);
    built.body.appendChild(technical);

    var trust = document.createElement('div');
    trust.className = 'channel-sheet-trust-note';
    trust.innerHTML = profile.authenticated_session
        ? '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/></svg><span>The destination and Link are authenticated and encrypted. The hub operator can still read, moderate, and relay everything posted to its channels.</span>'
        : '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/></svg><span>Ratspeak authenticates the destination and Link before any channel action. This view is not proof of an active session.</span>';
    built.body.appendChild(trust);

    var copyButton = document.createElement('button');
    copyButton.type = 'button';
    copyButton.className = 'nr-btn nr-btn-secondary';
    copyButton.textContent = 'Copy address';
    copyButton.addEventListener('click', function() {
        RS.copyText(hub.destination_hash).then(function(ok) {
            if (typeof showToast === 'function') showToast(ok ? 'Hub address copied' : 'Could not copy', ok ? 'toast-success' : 'toast-error', 1800);
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
    var hubActions = document.createElement('div');
    hubActions.className = 'channel-hub-profile-actions';
    hubActions.appendChild(shareButton);
    hubActions.appendChild(copyButton);
    built.body.insertBefore(hubActions, technical);

    var close = document.createElement('button');
    close.type = 'button';
    close.className = 'nr-btn nr-btn-secondary';
    close.textContent = 'Close';
    close.addEventListener('click', built.dismiss);
    var disconnect = document.createElement('button');
    disconnect.type = 'button';
    disconnect.className = 'nr-btn nr-btn-danger';
    disconnect.textContent = _channelsIsConnecting() ? 'Cancel connection' : 'Disconnect';
    disconnect.addEventListener('click', function() {
        var canceling = _channelsIsConnecting();
        var proceed = typeof rsConfirm === 'function'
            ? rsConfirm({
                title: canceling ? 'Cancel connection?' : 'Disconnect from ' + profile.display_name + '?',
                message: canceling
                    ? 'Stop this connection attempt.'
                    : 'Live channel sessions will end. Local history stays on this device.',
                confirmText: canceling ? 'Cancel connection' : 'Disconnect',
                danger: true
            })
            : Promise.resolve(true);
        proceed.then(function(confirmed) {
            if (!confirmed) return;
            _channelsDisconnectFromHub(disconnect).then(function(snapshot) {
                if (snapshot) built.dismiss();
            });
        });
    });
    built.footer.appendChild(close);
    built.footer.appendChild(disconnect);
    _channelsPresentSheet(built, copyButton);
}

function channelsOpenRoomOptions(eventOrTrigger) {
    var room = _channelsSelectedRoomView();
    var context = _channelsHistoryContext(room);
    if (!room || !context) return;
    var trigger = eventOrTrigger && eventOrTrigger.currentTarget
        ? eventOrTrigger.currentTarget
        : null;
    if (!trigger || !RS.ui || typeof RS.ui.openActionMenu !== 'function') {
        channelsOpenRoomDetails();
        return;
    }
    var items = [
        {
            label: 'Channel settings',
            icon: _channelsActionIcon('info'),
            onSelect: channelsOpenRoomDetails
        },
        {
            label: 'Share channel',
            icon: _channelsActionIcon('share'),
            onSelect: function() {
                channelsOpenChannelShare(
                    context.hub_destination_hash,
                    context.room_name
                );
            }
        }
    ];

    if (room.history_only) {
        var activeHub = channelsSnapshot.hub && channelsSnapshot.hub.destination_hash;
        var sameHub = _channelsIsConnected() &&
            String(activeHub || '').toLowerCase() === context.hub_destination_hash;
        items.push({ separator: true });
        items.push({
            label: sameHub ? 'Rejoin channel' : 'Connect to hub',
            icon: _channelsActionIcon('leave'),
            onSelect: function() {
                if (sameHub) channelsOpenJoinSheet(context.room_name);
                else channelsOpenConnectSheet(_channelsHubByDestination(context.hub_destination_hash));
            }
        });
    } else {
        items.push({ separator: true });
        items.push({
            label: room.phase === 'joining' ? 'Cancel join' : 'Leave channel',
            icon: _channelsActionIcon('leave'),
            danger: room.phase === 'joined',
            disabled: room.phase === 'parting',
            onSelect: function() {
                var proceed = room.phase === 'joined' && typeof rsConfirm === 'function'
                    ? rsConfirm({
                        title: 'Leave ' + _channelsRoomDisplayName(room.name) + '?',
                        message: 'Live membership will end. Local history stays on this device.',
                        confirmText: 'Leave channel',
                        danger: true
                    })
                    : Promise.resolve(true);
                proceed.then(function(confirmed) {
                    if (!confirmed) return;
                    _channelsPartRoom(room.name).catch(function(err) {
                        if (typeof showToast === 'function') {
                            showToast((err && err.message) || 'Could not leave channel', 'toast-error', 3200);
                        }
                    });
                });
            }
        });
    }
    RS.ui.openActionMenu(trigger, items, {
        title: _channelsRoomDisplayName(room.name),
        showTitle: false
    });
}

function channelsOpenRoomDetails() {
    var room = _channelsSelectedRoomView();
    var context = _channelsHistoryContext(room);
    if (!room || !context || typeof _rsBuildSheet !== 'function') return;
    var built = _rsBuildSheet({ title: _channelsRoomDisplayName(room.name) }, function() {});
    built.sheet.classList.add('channel-room-options-sheet');
    var copy = document.createElement('p');
    copy.className = 'channel-sheet-copy';
    if (room.history_only) {
        copy.textContent = 'Not currently joined.';
    } else if (room.phase === 'joining') {
        copy.textContent = 'Waiting for the hub to confirm your join.';
    } else if (room.phase === 'error') {
        copy.textContent = room.last_error || 'The hub did not confirm this join. Try again without reconnecting, or leave this channel.';
    }
    if (copy.textContent) built.body.appendChild(copy);
    if ((room.phase === 'joined' || room.history_only) &&
            (room.registered != null || room.topic || room.modes)) {
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
            policyNote.textContent = 'Notify for every new post.';
        } else if (notificationSelect.value === 'mute') {
            policyNote.textContent = 'Keep unread counts without alerts.';
        } else {
            policyNote.textContent = 'Notify only when you are mentioned.';
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
        }).catch(function(error) {
            notificationSelect.value = previous;
            renderPolicyNote();
            if (typeof showToast === 'function') {
                showToast((error && error.message) || 'Could not update channel notifications', 'toast-error', 3200);
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
        var proceed = room.phase === 'joined' && typeof rsConfirm === 'function'
            ? rsConfirm({
                title: 'Leave ' + _channelsRoomDisplayName(room.name) + '?',
                message: 'Live membership will end. Local history stays on this device.',
                confirmText: 'Leave channel',
                danger: true
            })
            : Promise.resolve(true);
        proceed.then(function(confirmed) {
            if (!confirmed) return;
            leave.disabled = true;
            _channelsPartRoom(room.name).then(function() {
                built.dismiss();
            }).catch(function(err) {
                leave.disabled = false;
                if (typeof showToast === 'function') showToast((err && err.message) || 'Could not leave channel', 'toast-error', 3200);
            });
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
        if (typeof showToast === 'function') showToast((error && error.message) || 'Could not end channel session', 'toast-error', 3500);
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
        if (typeof showToast === 'function') showToast('Channel message exceeds the hub limit', 'toast-warning', 3000);
        return;
    }
    var shouldRestoreComposerFocus = RS.composer
        ? RS.composer.consumeFocus(input)
        : document.activeElement === input;
    _channelsSendPending = true;
    _channelsUpdateComposer();
    var request = RS.invoke('send_channel_message', {
        args: { room: room.name, text: text }
    });

    // Match Direct Messages: finish the local composer interaction before the
    // asynchronous command result so Android's IME never closes and reopens.
    if (typeof RS !== 'undefined' && RS.composer && typeof RS.composer.reset === 'function') RS.composer.reset(input);
    else {
        input.value = '';
        input.style.height = '';
        input.scrollTop = 0;
    }
    if (shouldRestoreComposerFocus && RS.composer) {
        RS.composer.focusWithoutScroll(input);
    }
    _channelsUpdateComposer();
    if (document.documentElement.classList.contains('keyboard-open') && RS.chatScroll) {
        RS.chatScroll.pinToBottom(_channelsEl('channel-transcript'));
    }

    request.then(function(result) {
        return _channelsHandleComposerResult(result, room.name);
    }).catch(function(error) {
        if (!input.value) {
            input.value = text;
            if (typeof RS !== 'undefined' && RS.composer && typeof RS.composer.resize === 'function') RS.composer.resize(input);
            else {
                input.style.height = 'auto';
                input.style.height = Math.min(input.scrollHeight, 124) + 'px';
            }
            if (document.documentElement.classList.contains('keyboard-open') && RS.chatScroll) {
                RS.chatScroll.pinToBottom(_channelsEl('channel-transcript'));
            }
        }
        if (typeof showToast === 'function') showToast((error && error.message) || 'Could not send channel message', 'toast-error', 3500);
    }).then(function() {
        _channelsSendPending = false;
        _channelsUpdateComposer();
    });
}

function _channelsBindUI() {
    document.addEventListener('click', function(event) {
        var actionEl = event.target.closest && event.target.closest('[data-channel-action]');
        if (!actionEl) return;
        var action = actionEl.dataset.channelAction;
        if (action === 'add' || action === 'manage-hub') {
            if (typeof channelsOpenAddSheet === 'function') channelsOpenAddSheet();
            else channelsOpenConnectSheet();
        } else if (action === 'hub-actions') {
            if (channelsSnapshot.hub) channelsOpenHubOptions(actionEl);
            else if (typeof channelsOpenAddSheet === 'function') channelsOpenAddSheet();
            else channelsOpenConnectSheet();
        } else if (action === 'connect') channelsOpenHubSwitcher();
        else if (action === 'open-owned-hub' && typeof channelHubOpenOwnHub === 'function') channelHubOpenOwnHub();
        else if (action === 'join') channelsOpenJoinSheet();
        else if (action === 'hub-info') channelsOpenHubOptions();
        else if (action === 'disconnect') channelsDisconnect();
        else if (action === 'retry-room') channelsOpenJoinSheet(actionEl.dataset.room || '');
        else if (action === 'leave-room') {
            actionEl.disabled = true;
            _channelsPartRoom(actionEl.dataset.room || '').catch(function(error) {
                actionEl.disabled = false;
                if (typeof showToast === 'function') showToast((error && error.message) || 'Could not leave channel', 'toast-error', 3200);
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
    var hubStrip = _channelsEl('channel-hub-strip');
    if (hubStrip) hubStrip.addEventListener('animationend', function(event) {
        if (event.animationName === 'channelHubSignalLap') {
            hubStrip.classList.remove('link-arrived');
        }
    });
    var hubMenu = _channelsEl('channel-hub-menu-btn');
    if (hubMenu) hubMenu.addEventListener('click', function() {
        if (typeof channelsOpenAddSheet === 'function') channelsOpenAddSheet();
        else channelsOpenConnectSheet();
    });
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
    var membersInfo = _channelsEl('channel-members-info');
    if (membersInfo && RS.ui && typeof RS.ui.bindHelpPopovers === 'function') {
        RS.ui.bindHelpPopovers(_channelsEl('channel-members-pane'));
    }
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
        var channelGrowRaf = null;
        RS.composer.bindTypingPolicy(input);
        input.addEventListener('input', function() {
            var previousHeight = input.style.height;
            if (typeof RS !== 'undefined' && RS.composer && typeof RS.composer.resize === 'function') RS.composer.resize(input);
            else {
                input.style.height = 'auto';
                input.style.height = Math.min(input.scrollHeight, 124) + 'px';
            }
            _channelsUpdateComposer();
            if (previousHeight !== input.style.height &&
                    document.documentElement.classList.contains('keyboard-open') &&
                    typeof _chatMessagesNearBottomForKeyboard === 'function' &&
                    _chatMessagesNearBottomForKeyboard()) {
                if (channelGrowRaf) cancelAnimationFrame(channelGrowRaf);
                channelGrowRaf = requestAnimationFrame(function() {
                    channelGrowRaf = null;
                    if (RS.chatScroll) {
                        RS.chatScroll.pinToBottom(_channelsEl('channel-transcript'));
                    }
                });
            }
        });
        input.addEventListener('keydown', function(event) {
            if (event.key === 'Enter' && !event.shiftKey && !event.isComposing && !isMobile()) {
                event.preventDefault();
                channelsSendMessage();
            }
        });
    }
    var send = _channelsEl('channel-send-btn');
    if (send && input && RS.composer) {
        RS.composer.bindTapToSend(send, input, channelsSendMessage);
    } else if (send) {
        send.addEventListener('click', channelsSendMessage);
    }
    // Hydrate global attention even when the Channels view has not been opened;
    // the writer's startup event can precede WebView listener registration.
    channelsRefreshUnread();
    renderChannels();
}

RS.listen('channels_snapshot', function(snapshot) {
    channelsApplySnapshot(snapshot);
});

RS.listen('app_settings_updated', _channelsApplyPublicConsentSettings);

RS.listen('contact_blocked', function(data) {
    var hash = String(data && data.hash || '').trim().toLowerCase();
    if (hash) _channelsBlockedAddresses[hash] = true;
    _channelsRenderAfterSafetyChange();
});

RS.listen('contact_unblocked', function() {
    _channelsLoadBlockedContacts().then(_channelsRenderAfterSafetyChange).catch(function() {});
});

// Treat push payloads as invalidation signals. A request sequence around the
// follow-up DB read prevents an older event or command response from replacing
// newer unread state on the independent Tauri delivery paths.
RS.listen('channels_unread', function() {
    channelsRefreshUnread();
});

if (typeof PeersCache !== 'undefined' && PeersCache &&
        typeof PeersCache.subscribe === 'function') {
    PeersCache.subscribe(_channelsRefreshMemberNamesFromPeers);
}

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
    _channelsSelectedMemberKey = null;
    _channelsMemberReturnFocusKey = null;
    _channelsSavedRoomsHub = null;
    _channelsSaveHubKey = null;
    _channelsSaveHubPromise = null;
    _channelsSavedRoomKeys = {};
    _channelsHistoryCache = {};
    _channelsParticipantRequestSeq++;
    _channelsHistoryEpoch++;
    _channelsResetMemberObservations();
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

document.addEventListener('DOMContentLoaded', function() {
    _channelsLoadPublicConsent().catch(function() {});
    _channelsLoadBlockedContacts().then(_channelsRenderAfterSafetyChange).catch(function() {});
    _channelsBindUI();
});
