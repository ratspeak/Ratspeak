// Outbound message states by phase, the same table as ratspeak-core's
// OutboundState. 'progress' steps come with transfer progress and are never stored.
var LXMF_OUTBOUND_STATES = {
    sending: 'flight',
    routing: 'flight',
    propagating: 'flight',
    link_establishing: 'flight',
    sending_via_link: 'flight',
    reusing_direct_link: 'flight',
    reusing_backchannel: 'flight',
    sent: 'flight',
    delivered: 'done',
    propagated: 'done',
    failed: 'ended',
    cancelled: 'ended',
    rejected: 'done',
    timeout: 'ended',
    paper: 'done',
    waiting: 'waiting',
    resource_link_ready: 'progress',
    resource_advertised: 'progress',
    resource_transferring: 'progress',
    resource_waiting_for_proof: 'progress'
};

function _isTerminalMessageState(state) {
    var phase = LXMF_OUTBOUND_STATES[state];
    return phase === 'done' || phase === 'ended';
}

function _isUnsettledMessageState(state) {
    var phase = LXMF_OUTBOUND_STATES[state];
    return phase === 'flight' || phase === 'progress';
}

// A step that shows a message on its way; the send itself sets `sending`.
function _isInFlightStep(step) {
    return step !== 'sending' && _isUnsettledMessageState(step);
}

var lxmfIdentity = null;
var lxmfContacts = [];
var lxmfConversations = [];
var lxmfActiveContact = null;
// FIELD_THREAD hex of the open conversation with lxmfActiveContact; '' is Main.
var lxmfActiveThread = '';
// get_conversation's `threads` payload for lxmfActiveContact: { items, next_number }.
var lxmfActiveThreads = null;
var _conversationEpoch = 0;
var _conversationIdentityGeneration = 0;
// Advanced by a bulk messages delete; a read from before it is dropped.
var _messagesClearedCount = 0;
var lxmfConversation = [];
var lxmfPendingFile = null;
var _ghostConversationHash = null;
var _replyTarget = null;
var _msgReactions = {};
var _lxmfDrafts = {};
var _conversationCache = {};
var _conversationCacheSizes = {};
var _conversationCacheBytes = 0;
var _cacheLru = [];
var _cacheMax = 8;
var _conversationCacheMaxBytes = 2 * 1024 * 1024;
var _imageBlobUrlCache = {};
var _imageBlobUrlLru = [];
var _imageBlobUrlMax = 64;
var _imageBlobUrlBytes = 0;
var _imageCacheGeneration = 0;
var _imageHydrationInFlight = {};
var _imageHydrationQueue = [];
var _imageHydrationActive = 0;
var _imageHydrationMax = 3;
var _imageHydrationObserver = null;
var _imageCacheIdentityHash = null;
var _messageLongPressDetachFns = [];
var _pendingAttachmentToken = 0;
var _attachmentRetirement = Promise.resolve();
var _pendingLxmfCancelByClientId = {};
var _deferredConversationRenderOptions = null;
var _deferredConversationRenderOwnerHash = null;
var _deferredConversationRenderGeneration = 0;
var _deferredConversationRenderRelease = null;
// The first unread message when the open conversation was opened: { key, count, firstId, placed }.
var _unreadMarker = null;
// Inbound messages that arrived below the reader while they read history.
var _jumpNewIds = [];
var lxmfLimits = {
    max_attachment_bytes: 128000000,
    max_message_bytes: 134217727,
    efficient_resource_bytes: 1048575,
    image_size_prompt_bytes: 1000000,
    default_propagation_limit_kb: 256,
    propagation_transfer_limit_kb: null,
};

function _canonicalConversationHash(value) {
    return String(value == null ? '' : value).trim().toLowerCase();
}

function _canonicalConversationThread(value) {
    return String(value == null ? '' : value).trim().toLowerCase();
}

// Cache/draft key of one conversation; Main keeps the bare contact hash.
function _conversationKey(hash, thread) {
    hash = _canonicalConversationHash(hash);
    thread = _canonicalConversationThread(thread);
    return thread ? hash + '~' + thread : hash;
}

function _activeConversationKey() {
    return lxmfActiveContact ? _conversationKey(lxmfActiveContact, lxmfActiveThread) : '';
}

// `thread` undefined means the open conversation for the open contact, else Main.
function _resolveConversationThread(hash, thread) {
    if (thread !== undefined && thread !== null) return _canonicalConversationThread(thread);
    return _canonicalConversationHash(hash) === _canonicalConversationHash(lxmfActiveContact)
        ? lxmfActiveThread : '';
}

function _lxmfAttentionForeground() {
    if (window.RS && typeof RS.isAttentionForeground === 'function') {
        return !!RS.isAttentionForeground();
    }
    if (typeof _currentLifecycleForeground === 'function') {
        return !!_currentLifecycleForeground();
    }
    return typeof document === 'undefined' || !document.hidden;
}

function _isConversationActivelyVisible(hash, thread) {
    if (!_lxmfAttentionForeground()) return false;
    if (typeof currentView !== 'undefined' && currentView !== 'message') return false;
    return !!hash &&
        _canonicalConversationHash(hash) === _canonicalConversationHash(lxmfActiveContact) &&
        _resolveConversationThread(hash, thread) === lxmfActiveThread;
}

function _notifyConversationOwnerChanged(hash, reason) {
    if (window.RS && RS.voiceMemos && typeof RS.voiceMemos.onConversationChanged === 'function') {
        RS.voiceMemos.onConversationChanged(hash || null, reason || 'navigation');
    }
}

function _conversationOwnerSnapshot() {
    return {
        hash: _canonicalConversationHash(lxmfActiveContact),
        thread: lxmfActiveContact ? lxmfActiveThread : '',
        epoch: _conversationEpoch,
        identityGeneration: _conversationIdentityGeneration,
    };
}

function _conversationOwnerIsCurrent(owner) {
    return !!owner &&
        _canonicalConversationHash(owner.hash) === _canonicalConversationHash(lxmfActiveContact) &&
        _canonicalConversationThread(owner.thread) === lxmfActiveThread &&
        owner.epoch === _conversationEpoch &&
        owner.identityGeneration === _conversationIdentityGeneration;
}

function _conversationOwnerIdentityIsCurrent(owner) {
    return !!owner && owner.identityGeneration === _conversationIdentityGeneration;
}

// `thread` undefined keeps the open conversation when the contact is unchanged,
// else opens Main.
function _activateConversation(hash, reason, thread) {
    var next = _canonicalConversationHash(hash);
    var current = _canonicalConversationHash(lxmfActiveContact);
    var nextThread = next ? _resolveConversationThread(next, thread) : '';
    if (next === current && nextThread === lxmfActiveThread) {
        lxmfActiveContact = next || null;
        return _conversationOwnerSnapshot();
    }
    _conversationEpoch += 1;
    if (next !== current) lxmfActiveThreads = null;
    lxmfActiveContact = next || null;
    lxmfActiveThread = nextThread;
    _notifyConversationOwnerChanged(next, reason || 'navigation');
    return _conversationOwnerSnapshot();
}

function _resetConversationSession(reason) {
    _conversationIdentityGeneration += 1;
    _conversationEpoch += 1;
    lxmfActiveContact = null;
    lxmfActiveThread = '';
    lxmfActiveThreads = null;
    if (RS.messageSearch) RS.messageSearch.reset();
    _notifyConversationOwnerChanged(null, reason || 'identity_replaced');
}

window.RS = window.RS || {};
RS.conversationOwner = {
    canonicalHash: _canonicalConversationHash,
    key: _conversationKey,
    snapshot: _conversationOwnerSnapshot,
    isCurrent: _conversationOwnerIsCurrent,
    isIdentityCurrent: _conversationOwnerIdentityIsCurrent,
    activate: _activateConversation,
    reset: _resetConversationSession,
};

function _detachMessageLongPressHandlers() {
    while (_messageLongPressDetachFns.length) {
        var detach = _messageLongPressDetachFns.pop();
        try { detach(); } catch (e) {}
    }
}
var lxstVoiceState = {
    available: false,
    running: false,
    active: null,
    incoming: null,
    audioRunning: false,
    audioMicrophone: false,
    audioSpeaker: false,
    microphoneMuted: false,
    speakerphone: false,
    lastAudioWarningKey: null,
    lastDialHash: null,
    lastError: null,
    establishedAtMs: null,
    establishedLinkId: null
};
var _voiceElapsedTimer = null;
var _voiceRingtoneTimeoutInFlight = false;
var _voiceSuppressNoAnswerCueUntil = 0;
var _voiceNativeAudioRouteToken = null;
var _voiceNativeAudioRoutePrimed = false;
var _voiceNativeAudioRouteLastSyncAt = 0;
var _voiceSpeakerRestartToken = 0;
var _voiceDialToken = 0;
var _voiceAnswerToken = 0;

function _voiceStatusLabel(status) {
    switch (status) {
        case 'calling': return 'Calling';
        case 'available': return 'Calling';
        case 'ringing': return 'Ringing';
        case 'answering': return 'Answering';
        case 'connecting': return 'Connecting';
        case 'ending': return 'Ending call';
        case 'established': return 'In call';
        case 'busy': return 'Busy';
        case 'rejected': return 'Rejected';
        default: return status ? String(status) : 'Call';
    }
}

function _voiceIcon(name, size) {
    var dim = size || 18;
    var attrs = 'width="' + dim + '" height="' + dim + '" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"';
    if (name === 'phone-off') {
        return '<svg ' + attrs + '><path d="M10.1 13.9a16 16 0 0 0 4.21 2.01l1.21-1.2a2 2 0 0 1 2.11-.45c.85.3 1.74.51 2.65.63A2 2 0 0 1 22 16.92v3a2 2 0 0 1-2.18 2 19.8 19.8 0 0 1-8.63-3.07 19.5 19.5 0 0 1-6-6A19.8 19.8 0 0 1 2.12 4.2 2 2 0 0 1 4.11 2h3a2 2 0 0 1 2 1.72c.12.91.33 1.8.63 2.65"/><line x1="2" y1="2" x2="22" y2="22"/></svg>';
    }
    if (name === 'phone-incoming') {
        return '<svg ' + attrs + '><polyline points="16 2 16 8 22 8"/><line x1="22" y1="2" x2="16" y2="8"/><path d="M22 16.92v3a2 2 0 0 1-2.18 2 19.8 19.8 0 0 1-8.63-3.07 19.5 19.5 0 0 1-6-6A19.8 19.8 0 0 1 2.12 4.2 2 2 0 0 1 4.11 2h3a2 2 0 0 1 2 1.72c.12.91.33 1.8.63 2.65a2 2 0 0 1-.45 2.11L8.09 9.69a16 16 0 0 0 6.22 6.22l1.21-1.2a2 2 0 0 1 2.11-.45c.85.3 1.74.51 2.65.63A2 2 0 0 1 22 16.92z"/></svg>';
    }
    if (name === 'mic') {
        return '<svg ' + attrs + '><path d="M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3z"/><path d="M19 10v2a7 7 0 0 1-14 0v-2"/><line x1="12" y1="19" x2="12" y2="22"/><line x1="8" y1="22" x2="16" y2="22"/></svg>';
    }
    if (name === 'speaker') {
        return '<svg ' + attrs + '><polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5"/></svg>';
    }
    if (name === 'speaker-on') {
        return '<svg ' + attrs + '><polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5"/><path d="M15.54 8.46a5 5 0 0 1 0 7.07"/><path d="M19.07 4.93a10 10 0 0 1 0 14.14"/></svg>';
    }
    return '<svg ' + attrs + '><path d="M22 16.92v3a2 2 0 0 1-2.18 2 19.8 19.8 0 0 1-8.63-3.07 19.5 19.5 0 0 1-6-6A19.8 19.8 0 0 1 2.12 4.2 2 2 0 0 1 4.11 2h3a2 2 0 0 1 2 1.72c.12.91.33 1.8.63 2.65a2 2 0 0 1-.45 2.11L8.09 9.69a16 16 0 0 0 6.22 6.22l1.21-1.2a2 2 0 0 1 2.11-.45c.85.3 1.74.51 2.65.63A2 2 0 0 1 22 16.92z"/></svg>';
}

function _voicePrimaryActionLabel(hash) {
    if (_voiceIncomingMatchesContact(hash)) {
        return lxstVoiceState.incoming.status === 'ringing' ? 'Answer call' : 'Hang up';
    }
    if (_voiceActiveMatchesContact(hash)) {
        var active = lxstVoiceState.active;
        return active && active.status === 'established' ? 'Hang up' : 'Cancel call';
    }
    return 'Call';
}

function _voicePrimaryActionIcon(hash) {
    if (_voiceIncomingMatchesContact(hash)) return _voiceIcon('phone-incoming', 18);
    if (_voiceActiveMatchesContact(hash)) return _voiceIcon('phone', 18);
    return _voiceIcon('phone', 18);
}

function _voiceRunPrimaryAction(hash) {
    if (_voiceIncomingMatchesContact(hash)) {
        return lxstVoiceState.incoming.status === 'ringing' ? _voiceAnswerCall() : _voiceHangupCall();
    }
    if (_voiceActiveMatchesContact(hash)) return _voiceHangupCall();
    return _voiceStartCall(hash);
}

function _voiceActionState(hash) {
    if (!lxstVoiceState.available || !hash) return { available: false };
    var activeMatches = _voiceActiveMatchesContact(hash);
    var incomingMatches = _voiceIncomingMatchesContact(hash);
    var busyElsewhere =
        (!!lxstVoiceState.active && !activeMatches) ||
        (!!lxstVoiceState.incoming && !incomingMatches);
    return {
        available: true,
        disabled: busyElsewhere,
        danger: activeMatches || (incomingMatches && lxstVoiceState.incoming.status !== 'ringing'),
        label: busyElsewhere ? 'Call in Progress' : _voicePrimaryActionLabel(hash),
        icon: busyElsewhere ? _voiceIcon('phone', 18) : _voicePrimaryActionIcon(hash)
    };
}

function voiceActionButtonHtml(id, hash) {
    var action = _voiceActionState(hash);
    if (!action.available) return '';
    var className = action.danger
        ? 'danger-btn entity-action-btn entity-action-call is-hangup'
        : 'nr-btn nr-btn-primary entity-action-btn entity-action-call';
    var disabled = action.disabled ? ' disabled aria-disabled="true"' : '';
    return '<button type="button" class="' + className + '" id="' + id + '" data-hash="' + escapeHtml(hash) + '"' + disabled + '>' +
        action.icon +
        '<span>' + escapeHtml(action.label) + '</span>' +
    '</button>';
}

function wireVoiceActionButton(id, beforeAction) {
    var btn = document.getElementById(id);
    if (!btn) return;
    btn.addEventListener('click', function(e) {
        if (e) {
            e.preventDefault();
            e.stopPropagation();
        }
        if (btn.disabled) return;
        _voiceBlockMobileNavigation(280);
        var hash = btn.dataset.hash;
        if (typeof beforeAction === 'function') beforeAction();
        _voiceRunPrimaryAction(hash);
    });
}

function _voiceAudioIssueLabel() {
    var active = lxstVoiceState.active;
    if (!active || active.status !== 'established') return '';
    if (lxstVoiceState.audioRunning) {
        if (lxstVoiceState.audioMicrophone && lxstVoiceState.audioSpeaker) return '';
        if (!lxstVoiceState.audioMicrophone && lxstVoiceState.audioSpeaker) return 'no microphone';
        if (lxstVoiceState.audioMicrophone && !lxstVoiceState.audioSpeaker) return 'no speaker';
        return 'no audio';
    }
    if (lxstVoiceState.lastAudioWarningKey) return 'no audio';
    if (lxstVoiceState.lastError && String(lxstVoiceState.lastError).indexOf('Failed to start LXST audio') !== -1) {
        return 'no audio';
    }
    return '';
}

function _voiceElapsedLabel() {
    var active = lxstVoiceState.active;
    if (!active || active.status !== 'established' || !lxstVoiceState.establishedAtMs) return '';
    var elapsed = Math.max(1, Math.floor((Date.now() - lxstVoiceState.establishedAtMs) / 1000) + 1);
    var minutes = Math.floor(elapsed / 60);
    var seconds = elapsed % 60;
    return minutes + ':' + (seconds < 10 ? '0' : '') + seconds;
}

function _voiceActiveStatusLabel(active) {
    var status = _voiceStatusLabel(active.status);
    var elapsed = _voiceElapsedLabel();
    if (elapsed) status += ' - ' + elapsed;
    return status;
}

function _voiceGlobalStatusLabel(active) {
    if (!active) return '';
    var audioIssue = _voiceAudioIssueLabel();
    if (audioIssue) return audioIssue;
    if (active.status === 'established') {
        var elapsed = _voiceElapsedLabel();
        return 'Active call' + (elapsed ? ' - ' + elapsed : '');
    }
    return _voiceStatusLabel(active.status);
}

function _voiceSyncElapsedTimer() {
    var active = lxstVoiceState.active;
    var shouldRun = !!(active && active.status === 'established' && lxstVoiceState.establishedAtMs);
    if (shouldRun && !_voiceElapsedTimer) {
        _voiceElapsedTimer = setInterval(renderVoiceUi, 1000);
    } else if (!shouldRun && _voiceElapsedTimer) {
        clearInterval(_voiceElapsedTimer);
        _voiceElapsedTimer = null;
    }
}

function _voiceResetCallControls() {
    _voiceStopTalk();
    _voiceSpeakerRestartToken++;
    lxstVoiceState.microphoneMuted = false;
    lxstVoiceState.speakerphone = false;
}

function _voiceTrackEstablished(active) {
    if (!active || active.status !== 'established') {
        lxstVoiceState.establishedAtMs = null;
        lxstVoiceState.establishedLinkId = null;
        _voiceResetCallControls();
        _voiceSyncElapsedTimer();
        return;
    }
    if (lxstVoiceState.establishedLinkId !== active.link_id || !lxstVoiceState.establishedAtMs) {
        lxstVoiceState.establishedLinkId = active.link_id;
        lxstVoiceState.establishedAtMs = Date.now();
        _voiceResetCallControls();
    }
    _voiceSyncElapsedTimer();
}

function _androidCallRouteBridge() {
    if (!window.RatspeakAndroid) return null;
    if (typeof window.RatspeakAndroid.primeCallAudioRoute !== 'function') return null;
    if (typeof window.RatspeakAndroid.startCallAudioRoute !== 'function') return null;
    if (typeof window.RatspeakAndroid.stopCallAudioRoute !== 'function') return null;
    return window.RatspeakAndroid;
}

function _voiceNativeRouteName() {
    return lxstVoiceState.speakerphone ? 'speaker' : 'earpiece';
}

function _voicePrimeNativeCallRoute() {
    var bridge = _androidCallRouteBridge();
    if (!bridge) return;
    var route = _voiceNativeRouteName();
    _voiceNativeAudioRoutePrimed = true;
    _voiceNativeAudioRouteToken = 'pending:' + route;
    try {
        bridge.primeCallAudioRoute(route);
        _voiceNativeAudioRouteLastSyncAt = Date.now();
    } catch (_) {
        _voiceNativeAudioRouteLastSyncAt = 0;
    }
}

function _voiceReleaseNativeCallRoutePrime() {
    _voiceNativeAudioRoutePrimed = false;
}

function _voiceSyncNativeAudioRoute(force) {
    var bridge = _androidCallRouteBridge();
    var active = lxstVoiceState.active;
    var shouldRoute = !!(active || _voiceNativeAudioRoutePrimed);
    if (!bridge) {
        _voiceNativeAudioRouteToken = null;
        _voiceNativeAudioRouteLastSyncAt = 0;
        return;
    }
    if (!shouldRoute) {
        if (_voiceNativeAudioRouteToken) {
            _voiceNativeAudioRouteToken = null;
            _voiceNativeAudioRouteLastSyncAt = 0;
            try { bridge.stopCallAudioRoute(); } catch (_) {}
        }
        return;
    }
    var route = _voiceNativeRouteName();
    var token = ((active && active.link_id) || 'pending') + ':' + route;
    var now = Date.now();
    if (!force && _voiceNativeAudioRouteToken === token && (now - _voiceNativeAudioRouteLastSyncAt) < 10000) return;
    _voiceNativeAudioRouteToken = token;
    _voiceNativeAudioRouteLastSyncAt = now;
    try {
        if (active && active.link_id) bridge.startCallAudioRoute(route, active.link_id);
        else bridge.primeCallAudioRoute(route);
    } catch (_) {}
}

function _voiceEnsureMicrophonePermission() {
    if (!window.RS || !RS.mediaPermissions || typeof RS.mediaPermissions.ensure !== 'function') {
        return Promise.resolve(true);
    }
    return RS.mediaPermissions.ensure({ audio: true }).then(function(granted) {
        if (!granted) {
            _voiceNotify('Microphone unavailable or permission denied; call will be listen only');
        }
        return true;
    });
}

function _voiceEnsurePlaybackReady() {
    // Android ringtones and LXST playback use native AudioTracks. Opening a
    // second Web Audio destination here can re-energize the speaker when the
    // Activity resumes and provides no audio for the call itself.
    if (_androidCallRouteBridge()) return Promise.resolve(true);
    if (!window.RS || !RS.audioPlayback || typeof RS.audioPlayback.ensure !== 'function') {
        return Promise.resolve(true);
    }
    return RS.audioPlayback.ensure({ installUnlock: true }).then(function() {
        return true;
    }).catch(function() {
        return true;
    });
}

function _voiceSyncRingtone() {
    if (!window.RS || !RS.ringtones || typeof RS.ringtones.sync !== 'function') return;
    RS.ringtones.sync(lxstVoiceState);
}

function _voiceStopRingtone() {
    if (!window.RS || !RS.ringtones || typeof RS.ringtones.stop !== 'function') return;
    RS.ringtones.stop();
}

function _voicePlayNoAnswerCue() {
    if (!window.RS || !RS.ringtones || typeof RS.ringtones.playTimeoutCue !== 'function') return;
    RS.ringtones.playTimeoutCue();
}

function _voiceHandleRingtoneTimeout() {
    var active = lxstVoiceState.active;
    if (_voiceRingtoneTimeoutInFlight || !active || active.role !== 'outgoing' || active.status === 'established') return;
    _voiceRingtoneTimeoutInFlight = true;
    _voiceSuppressNoAnswerCueUntil = Date.now() + 2000;
    RS.invoke('voice_hangup').catch(function(err) {
        window.RS.diag('warn', '[lxst] outgoing ringtone timeout hangup failed:', err);
    }).then(function() {
        _voiceRingtoneTimeoutInFlight = false;
        if (!lxstVoiceState.active || lxstVoiceState.active.status === 'established') return;
        lxstVoiceState.active = null;
        lxstVoiceState.incoming = null;
        lxstVoiceState.audioRunning = false;
        lxstVoiceState.audioMicrophone = false;
        lxstVoiceState.audioSpeaker = false;
        _voiceReleaseNativeCallRoutePrime();
        _voiceResetCallControls();
        lxstVoiceState.lastDialHash = null;
        lxstVoiceState.establishedAtMs = null;
        lxstVoiceState.establishedLinkId = null;
        _voiceTrackEstablished(null);
        renderVoiceUi();
    });
}

if (window.RS && RS.ringtones && typeof RS.ringtones.setHandlers === 'function') {
    RS.ringtones.setHandlers({ onOutgoingTimeout: _voiceHandleRingtoneTimeout });
}

function _voicePeerLookupHash(call) {
    if (!call) return '';
    if (typeof call === 'string') return call;
    if (call.role === 'outgoing' && lxstVoiceState.lastDialHash) return lxstVoiceState.lastDialHash;
    return call.remote_lxmf_destination || call.remote_lxmf_hash || call.contact_hash || call.requested_hash || call.remote_identity || '';
}

function _voicePeerAddress(call) {
    if (!call) return '';
    if (typeof call === 'string') return call;
    return _voicePeerLookupHash(call) || call.remote_identity || '';
}

function _voicePeerDisplayInfo(call) {
    var lookupHash = _voicePeerLookupHash(call);
    var address = _voicePeerAddress(call);
    var avatarHash = lookupHash || address;
    if (lookupHash && typeof _conversationNameInfo === 'function') {
        var info = _conversationNameInfo(lookupHash, null, false);
        if (info && info.name && !info.isHash) {
            return {
                name: info.name,
                address: address || lookupHash,
                avatarHash: avatarHash,
                hasName: true
            };
        }
    }
    return {
        name: 'Unknown caller',
        address: address || lookupHash,
        avatarHash: avatarHash,
        hasName: false
    };
}

function _voicePeerName(call) {
    var info = _voicePeerDisplayInfo(call);
    if (info.hasName) return info.name;
    return info.address || 'Unknown caller';
}

function _voicePeerSurfaceTitle(call) {
    return _voicePeerName(call);
}

function _voiceCallSurfaceAvatarHtml(call, size) {
    var info = _voicePeerDisplayInfo(call);
    if (typeof identityAvatar === 'function') {
        return identityAvatar(info.avatarHash || info.address || '', size);
    }
    return _voiceIcon('phone', Math.max(18, Math.round(size * 0.45)));
}

function _voiceWireHangupProximity(surfaceId, hangupId) {
    var surface = document.getElementById(surfaceId);
    var hangup = document.getElementById(hangupId);
    if (!surface || !hangup || surface._voiceHangupProximityBound) return;
    surface._voiceHangupProximityBound = true;
    surface.addEventListener('click', function(e) {
        if (e.target && e.target.closest && e.target.closest('button')) return;
        if (!lxstVoiceState.active || hangup.style.display === 'none') return;
        var surfaceRect = surface.getBoundingClientRect();
        var buttonRect = hangup.getBoundingClientRect();
        var pad = 12;
        var x = e.clientX;
        var y = e.clientY;
        var inExpandedButton =
            x >= buttonRect.left - pad &&
            x <= buttonRect.right + pad &&
            y >= Math.max(surfaceRect.top, buttonRect.top - pad) &&
            y <= Math.min(surfaceRect.bottom, buttonRect.bottom + pad);
        if (inExpandedButton) _voiceHangupCall();
    });
}

function _voiceRenderCallSurface(ids) {
    var surface = document.getElementById(ids.surface);
    if (!surface) return;
    var active = lxstVoiceState.active;
    var incoming = lxstVoiceState.incoming;
    var titleEl = document.getElementById(ids.title);
    var statusEl = document.getElementById(ids.status);
    var answerBtn = document.getElementById(ids.answer);
    var rejectBtn = document.getElementById(ids.reject);
    var hangupBtn = document.getElementById(ids.hangup);
    var controls = ids.controls ? document.getElementById(ids.controls) : null;
    var muteBtn = ids.mute ? document.getElementById(ids.mute) : null;
    var speakerBtn = ids.speaker ? document.getElementById(ids.speaker) : null;
    var peer = active || incoming;
    var avatarEl = surface.querySelector('.lxst-call-strip-indicator');

    surface.hidden = !peer;
    surface.classList.toggle('is-ptt', !!(active && active.push_to_talk));
    surface.classList.toggle('is-incoming', !!incoming && !active);
    surface.classList.toggle('is-active', !!(active && active.status === 'established'));
    surface.classList.toggle('is-connecting', !!(
        (active && active.status !== 'established') ||
        (incoming && incoming.status !== 'ringing')
    ));
    if (!peer) return;

    if (avatarEl) {
        avatarEl.innerHTML = _voiceCallSurfaceAvatarHtml(peer, ids.global ? 48 : 42);
    }
    if (titleEl) {
        titleEl.textContent = _voicePeerSurfaceTitle(peer);
        titleEl.title = titleEl.textContent;
    }
    if (statusEl) {
        var status = active
            ? (ids.global ? _voiceGlobalStatusLabel(active) : _voiceActiveStatusLabel(active))
            : (incoming.status === 'ringing' ? 'Incoming call' : _voiceStatusLabel(incoming.status));
        var audioIssue = active && !ids.global ? _voiceAudioIssueLabel() : '';
        if (audioIssue) status += ' - ' + audioIssue;
        statusEl.textContent = status;
    }

    var showIncomingActions = !!incoming && !active && incoming.status === 'ringing';
    var showIncomingHangup = !!incoming && !active && incoming.status !== 'ringing';
    if (answerBtn) answerBtn.style.display = showIncomingActions ? '' : 'none';
    if (rejectBtn) rejectBtn.style.display = showIncomingActions ? '' : 'none';
    if (hangupBtn) {
        hangupBtn.style.display = active || showIncomingHangup ? '' : 'none';
        hangupBtn.innerHTML = _voiceIcon('phone', 16) + '<span>Hang up</span>';
    }

    var showCallControls = !!(active && active.status === 'established');
    if (controls) {
        controls.hidden = !showCallControls;
        var ptt = _voiceTalkButton(controls);
        ptt.hidden = !(showCallControls && active.push_to_talk);
        ptt.disabled = !lxstVoiceState.audioMicrophone;
        ptt.textContent = _voiceTalkHeld ? 'Talking — release to stop' : 'Hold to talk';
        ptt.setAttribute('aria-pressed', _voiceTalkHeld ? 'true' : 'false');
        ptt.title = 'Hold to talk, up to 10 seconds';
    }
    if (muteBtn) {
        var muteLabel = lxstVoiceState.microphoneMuted ? 'Unmute microphone' : 'Mute microphone';
        if (!lxstVoiceState.audioMicrophone) muteLabel = 'Microphone unavailable';
        muteBtn.style.display = showCallControls && !active.push_to_talk ? '' : 'none';
        muteBtn.disabled = showCallControls && !lxstVoiceState.audioMicrophone;
        muteBtn.classList.toggle('is-muted', !!lxstVoiceState.microphoneMuted);
        muteBtn.setAttribute('aria-pressed', lxstVoiceState.microphoneMuted ? 'true' : 'false');
        muteBtn.setAttribute('title', muteLabel);
        muteBtn.setAttribute('aria-label', muteLabel);
        muteBtn.innerHTML = _voiceIcon('mic', 18);
    }
    if (speakerBtn) {
        var canRouteSpeaker = !!_androidCallRouteBridge();
        var speakerLabel = lxstVoiceState.speakerphone ? 'Use earpiece' : 'Use speaker';
        speakerBtn.style.display = showCallControls && canRouteSpeaker ? '' : 'none';
        speakerBtn.disabled = !(showCallControls && canRouteSpeaker);
        speakerBtn.classList.toggle('is-on', !!lxstVoiceState.speakerphone);
        speakerBtn.setAttribute('aria-pressed', lxstVoiceState.speakerphone ? 'true' : 'false');
        speakerBtn.setAttribute('title', speakerLabel);
        speakerBtn.setAttribute('aria-label', speakerLabel);
        speakerBtn.innerHTML = _voiceIcon(lxstVoiceState.speakerphone ? 'speaker-on' : 'speaker', 18);
    }
}

function _voiceActiveMatchesContact(hash) {
    var active = lxstVoiceState.active;
    var target = hash || lxmfActiveContact;
    if (!active || !target) return false;
    return _voicePeerLookupHash(active) === target ||
        active.remote_identity === target ||
        lxstVoiceState.lastDialHash === target;
}

function _voiceIncomingMatchesContact(hash) {
    var incoming = lxstVoiceState.incoming;
    var target = hash || lxmfActiveContact;
    if (!incoming || !target) return false;
    return _voicePeerLookupHash(incoming) === target ||
        incoming.remote_identity === target;
}

function _voiceNotify(message, className) {
    if (typeof showToast === 'function') {
        showToast(message, className || 'toast-warning', 2500);
    }
}

function _voiceRestoreHeaderStatus() {
    _applyChatHeaderPresence();
}

function _voiceHaptic(name) {
    if (typeof haptic === 'function') haptic(name);
}

function _voiceBlockMobileNavigation(ms) {
    if (window.RS && typeof window.RS.blockMobileNavigation === 'function') {
        window.RS.blockMobileNavigation(ms || 280);
    }
}

function _voiceAfterNextPaint() {
    return new Promise(function(resolve) {
        if (typeof requestAnimationFrame === 'function') {
            requestAnimationFrame(function() { setTimeout(resolve, 0); });
        } else {
            setTimeout(resolve, 0);
        }
    });
}

function _voiceCancelPendingDial() {
    _voiceDialToken++;
}

function _voiceIncomingIsExact(linkId) {
    return !!(linkId && lxstVoiceState.incoming && lxstVoiceState.incoming.link_id === linkId);
}

function _voiceSetOptimisticOutgoing(hash) {
    lxstVoiceState.active = {
        link_id: null,
        remote_identity: null,
        remote_lxmf_destination: hash,
        requested_hash: hash,
        role: 'outgoing',
        status: 'calling'
    };
    lxstVoiceState.incoming = null;
    lxstVoiceState.lastError = null;
    lxstVoiceState.audioRunning = false;
    lxstVoiceState.audioMicrophone = false;
    lxstVoiceState.audioSpeaker = false;
    _voiceTrackEstablished(lxstVoiceState.active);
}

function _voiceCancelMemoForCall() {
    if (window.RS && RS.voiceMemos && typeof RS.voiceMemos.cancelForCall === 'function') {
        return RS.voiceMemos.cancelForCall();
    }
    return Promise.resolve();
}

function _voiceStartCall(hash) {
    if (!lxstVoiceState.available || !hash) return Promise.resolve();
    var dialToken = ++_voiceDialToken;
    _voiceHaptic('selection');
    _voiceBlockMobileNavigation(280);
    _voiceResetCallControls();
    lxstVoiceState.lastDialHash = hash;
    _voiceSetOptimisticOutgoing(hash);
    renderVoiceUi();
    return _voiceCancelMemoForCall().then(_voiceAfterNextPaint).then(_voiceEnsurePlaybackReady).then(_voiceEnsureMicrophonePermission).then(function() {
        if (dialToken !== _voiceDialToken || !_voiceActiveMatchesContact(hash)) return;
        _voicePrimeNativeCallRoute();
        return RS.invoke('voice_call', { args: { hash: hash } }).then(function(result) {
            if (dialToken !== _voiceDialToken || !_voiceActiveMatchesContact(hash)) return;
            if (result && result.requested_hash) lxstVoiceState.lastDialHash = result.requested_hash;
            renderVoiceUi();
        }).catch(function(err) {
            if (dialToken !== _voiceDialToken || !_voiceActiveMatchesContact(hash)) return;
            lxstVoiceState.lastDialHash = null;
            lxstVoiceState.active = null;
            lxstVoiceState.incoming = null;
            lxstVoiceState.audioRunning = false;
            lxstVoiceState.audioMicrophone = false;
            lxstVoiceState.audioSpeaker = false;
            _voiceReleaseNativeCallRoutePrime();
            _voiceHaptic('error');
            _voiceNotify((err && err.message) || 'Could not start call');
            renderVoiceUi();
        });
    }).catch(function(err) {
        if (dialToken !== _voiceDialToken || !_voiceActiveMatchesContact(hash)) return;
        lxstVoiceState.lastDialHash = null;
        lxstVoiceState.active = null;
        lxstVoiceState.incoming = null;
        lxstVoiceState.audioRunning = false;
        lxstVoiceState.audioMicrophone = false;
        lxstVoiceState.audioSpeaker = false;
        _voiceReleaseNativeCallRoutePrime();
        _voiceHaptic('error');
        _voiceNotify((err && err.message) || 'Could not start call');
        renderVoiceUi();
    });
}

function _voiceAnswerCall() {
    var incoming = lxstVoiceState.incoming;
    if (!incoming || !incoming.link_id || incoming.status !== 'ringing') return Promise.resolve();
    var expectedLinkId = incoming.link_id;
    var answerToken = ++_voiceAnswerToken;
    _voiceStopRingtone();
    _voiceHaptic('selection');
    incoming.status = 'answering';
    renderVoiceUi();
    return _voiceCancelMemoForCall().then(_voiceEnsurePlaybackReady).then(_voiceEnsureMicrophonePermission).then(function() {
        if (answerToken !== _voiceAnswerToken || !_voiceIncomingIsExact(expectedLinkId)) return;
        _voiceResetCallControls();
        _voicePrimeNativeCallRoute();
        return RS.invoke('voice_answer', { args: { link_id: expectedLinkId } }).then(function(result) {
            if (answerToken !== _voiceAnswerToken || !_voiceIncomingIsExact(expectedLinkId)) return;
            _voiceHaptic('success');
            // Command completion confirms exact call admission, not remote
            // establishment. Keep the call visible until the authoritative
            // same-link snapshot observes the peer's Established response.
            lxstVoiceState.incoming.status = (result && result.status) || 'connecting';
            renderVoiceUi();
        }).catch(function(err) {
            if (answerToken !== _voiceAnswerToken || !_voiceIncomingIsExact(expectedLinkId)) return;
            lxstVoiceState.incoming.status = 'ringing';
            _voiceReleaseNativeCallRoutePrime();
            _voiceHaptic('error');
            _voiceNotify((err && err.message) || 'Could not answer call');
            renderVoiceUi();
        });
    }).catch(function(err) {
        if (answerToken !== _voiceAnswerToken || !_voiceIncomingIsExact(expectedLinkId)) return;
        lxstVoiceState.incoming.status = 'ringing';
        _voiceReleaseNativeCallRoutePrime();
        _voiceHaptic('error');
        _voiceNotify((err && err.message) || 'Could not answer call');
        renderVoiceUi();
    });
}

function _voiceRejectCall() {
    _voiceStopRingtone();
    _voiceCancelPendingDial();
    _voiceHaptic('warning');
    _voiceSuppressNoAnswerCueUntil = Date.now() + 2000;
    return RS.invoke('voice_reject').catch(function() {});
}

function _voiceHangupCall() {
    _voiceStopTalk();
    _voiceStopRingtone();
    _voiceCancelPendingDial();
    _voiceAnswerToken++;
    _voiceHaptic('warning');
    _voiceSuppressNoAnswerCueUntil = Date.now() + 2000;
    var previousActiveStatus = lxstVoiceState.active && lxstVoiceState.active.status;
    var previousIncomingStatus = lxstVoiceState.incoming && lxstVoiceState.incoming.status;
    if (lxstVoiceState.active) lxstVoiceState.active.status = 'ending';
    if (lxstVoiceState.incoming) lxstVoiceState.incoming.status = 'ending';
    renderVoiceUi();
    return RS.invoke('voice_hangup').catch(function(err) {
        if (lxstVoiceState.active && previousActiveStatus) lxstVoiceState.active.status = previousActiveStatus;
        if (lxstVoiceState.incoming && previousIncomingStatus) lxstVoiceState.incoming.status = previousIncomingStatus;
        _voiceHaptic('error');
        _voiceNotify((err && err.message) || 'Could not hang up call');
        renderVoiceUi();
    });
}

// Constrained calls use a native, exact-Link PTT lease. Pointer/keyboard release,
// navigation and UI suspension revoke it; renewal cannot extend the 10s burst.
var _voiceTalkSerial = 0;
var _voiceTalkHeld = null;
function _voiceStopTalk() {
    var held = _voiceTalkHeld;
    if (!held) return;
    _voiceTalkHeld = null;
    clearInterval(held.timer);
    RS.invoke('voice_set_talk', { args: { link_id: held.link, serial: ++_voiceTalkSerial, pressed: false } }).catch(function() {});
    renderVoiceUi();
}
function _voiceStartTalk() {
    var active = lxstVoiceState.active;
    if (_voiceTalkHeld || !active || !active.push_to_talk || active.status !== 'established' || !lxstVoiceState.audioMicrophone) return;
    // A reloaded webview must advance the still-running native call's input
    // serial. Restarting at zero would leave every fresh press stale.
    if (Number.isSafeInteger(active.ptt_input_serial) && active.ptt_input_serial >= 0) {
        _voiceTalkSerial = Math.max(_voiceTalkSerial, active.ptt_input_serial);
    }
    if (_voiceTalkSerial >= Number.MAX_SAFE_INTEGER - 1) return;
    var held = { link: active.link_id, serial: ++_voiceTalkSerial, born: Date.now(), timer: null };
    _voiceTalkHeld = held;
    function renew() {
        var current = lxstVoiceState.active;
        if (_voiceTalkHeld !== held) return;
        if (!current || current.link_id !== held.link || current.status !== 'established' || document.hidden || Date.now() - held.born >= 10000) {
            _voiceStopTalk(); return;
        }
        RS.invoke('voice_set_talk', { args: { link_id: held.link, serial: held.serial, pressed: true } }).catch(function() {
            if (_voiceTalkHeld === held) _voiceStopTalk();
        });
    }
    held.timer = setInterval(renew, 250);
    renew(); renderVoiceUi();
}
function _voiceTalkButton(controls) {
    var button = controls.querySelector('.lxst-call-ptt');
    if (button) return button;
    button = document.createElement('button');
    button.type = 'button'; button.className = 'lxst-call-action lxst-call-ptt';
    button.style.touchAction = 'none';
    button.addEventListener('pointerdown', function(e) {
        if (e.button !== 0) return;
        e.preventDefault(); button.setPointerCapture(e.pointerId); _voiceStartTalk();
    });
    ['pointerup', 'pointercancel', 'lostpointercapture', 'blur'].forEach(function(name) {button.addEventListener(name, _voiceStopTalk);});
    button.addEventListener('keydown', function(e) {if (e.key === ' ' || e.key === 'Enter') {e.preventDefault();if (!e.repeat) _voiceStartTalk();}});
    button.addEventListener('keyup', function(e) {if (e.key === ' ' || e.key === 'Enter') {e.preventDefault();_voiceStopTalk();}});
    controls.appendChild(button);return button;
}
window.addEventListener('blur', _voiceStopTalk);
window.addEventListener('pagehide', _voiceStopTalk);
document.addEventListener('visibilitychange', function() {if (document.hidden) _voiceStopTalk();});

function _voiceToggleMute() {
    var active = lxstVoiceState.active;
    if (!active || active.status !== 'established' || !lxstVoiceState.audioMicrophone) return Promise.resolve();
    var nextMuted = !lxstVoiceState.microphoneMuted;
    var previousMuted = lxstVoiceState.microphoneMuted;
    lxstVoiceState.microphoneMuted = nextMuted;
    _voiceHaptic('selection');
    renderVoiceUi();
    return RS.invoke('voice_set_microphone_muted', { args: { muted: nextMuted } }).then(function(result) {
        if (result && typeof result.microphone_muted === 'boolean') {
            lxstVoiceState.microphoneMuted = result.microphone_muted;
        }
        renderVoiceUi();
    }).catch(function(err) {
        lxstVoiceState.microphoneMuted = previousMuted;
        _voiceHaptic('error');
        _voiceNotify((err && err.message) || 'Could not update microphone mute');
        renderVoiceUi();
    });
}

function _voiceToggleSpeaker() {
    var active = lxstVoiceState.active;
    if (!active || active.status !== 'established' || !_androidCallRouteBridge()) return;
    var previousSpeakerphone = lxstVoiceState.speakerphone;
    var nextSpeakerphone = !previousSpeakerphone;
    var restartToken = ++_voiceSpeakerRestartToken;
    lxstVoiceState.speakerphone = nextSpeakerphone;
    _voiceHaptic('selection');
    _voiceNativeAudioRouteToken = null;
    renderVoiceUi();
    setTimeout(function() {
        if (restartToken !== _voiceSpeakerRestartToken) return;
        RS.invoke('voice_restart_speaker', { args: { speakerphone: nextSpeakerphone } }).catch(function(err) {
            if (restartToken !== _voiceSpeakerRestartToken) return;
            lxstVoiceState.speakerphone = previousSpeakerphone;
            _voiceNativeAudioRouteToken = null;
            _voiceHaptic('error');
            _voiceNotify((err && err.message) || 'Could not switch audio route');
            renderVoiceUi();
        });
    }, 280);
}

function renderVoiceUi() {
    if (_voiceTalkHeld && (!lxstVoiceState.active || lxstVoiceState.active.link_id !== _voiceTalkHeld.link || lxstVoiceState.active.status !== 'established' || !lxstVoiceState.active.push_to_talk)) _voiceStopTalk();
    var callBtn = document.getElementById('lxst-call-btn');
    var active = lxstVoiceState.active;
    var incoming = lxstVoiceState.incoming;
    var activeMatches = _voiceActiveMatchesContact();
    var incomingMatches = _voiceIncomingMatchesContact();

    if (callBtn) {
        // A call with this person moves to the call strip; elsewhere it disables Call.
        var canShow = lxstVoiceState.available && !!lxmfActiveContact && !activeMatches && !incomingMatches;
        callBtn.hidden = !canShow;
        callBtn.disabled = !!active && !activeMatches;
        callBtn.title = callBtn.disabled ? 'Call in progress' : 'Call';
        var who = lxmfActiveContact ? _conversationNameInfo(lxmfActiveContact, null, false) : null;
        var first = who && !who.isHash ? String(who.name || '').trim().split(/\s+/)[0] : '';
        callBtn.setAttribute('aria-label', callBtn.disabled ? 'Call in progress' : (first ? 'Call ' + first : 'Call'));
    }

    _voiceRestoreHeaderStatus();

    _voiceRenderCallSurface({
        surface: 'lxst-call-strip',
        title: 'lxst-call-strip-title',
        status: 'lxst-call-strip-status',
        answer: 'lxst-call-answer-btn',
        reject: 'lxst-call-reject-btn',
        hangup: 'lxst-call-hangup-btn',
        controls: 'lxst-call-strip-controls',
        mute: 'lxst-call-mute-btn',
        speaker: 'lxst-call-speaker-btn'
    });
    _voiceRenderCallSurface({
        surface: 'lxst-call-global',
        title: 'lxst-call-global-title',
        status: 'lxst-call-global-status',
        answer: 'lxst-call-global-answer-btn',
        reject: 'lxst-call-global-reject-btn',
        hangup: 'lxst-call-global-hangup-btn',
        controls: 'lxst-call-global-controls',
        mute: 'lxst-call-global-mute-btn',
        speaker: 'lxst-call-global-speaker-btn',
        global: true
    });

    renderVoiceIncomingSheet();
    _voiceSyncElapsedTimer();
    _voiceSyncRingtone();
    _voiceSyncNativeAudioRoute();
}

function renderVoiceIncomingSheet() {
    var incoming = lxstVoiceState.incoming;
    var existing = document.getElementById('lxst-incoming-call-overlay');
    var sheet = document.getElementById('lxst-incoming-call-sheet');
    if (!incoming || incoming.status !== 'ringing') {
        if (existing) existing.remove();
        if (sheet) sheet.remove();
        return;
    }
    if (!existing || !sheet) {
        if (existing) existing.remove();
        if (sheet) sheet.remove();
        existing = document.createElement('div');
        existing.id = 'lxst-incoming-call-overlay';
        existing.className = 'bottom-sheet-overlay active lxst-incoming-call-overlay';
        sheet = document.createElement('div');
        sheet.id = 'lxst-incoming-call-sheet';
        sheet.className = 'bottom-sheet open lxst-incoming-call-sheet';
        sheet.setAttribute('role', 'dialog');
        sheet.setAttribute('aria-modal', 'true');
        sheet.setAttribute('aria-labelledby', 'lxst-incoming-call-title');
        sheet.innerHTML =
            '<div class="bottom-sheet-handle"></div>' +
            '<div class="bottom-sheet-header lxst-incoming-call-header">' +
                '<div class="bottom-sheet-title" id="lxst-incoming-call-title">Incoming call</div>' +
            '</div>' +
            '<div class="bottom-sheet-body lxst-incoming-call-body">' +
                '<div class="lxst-incoming-call-peer">' +
                    '<div class="lxst-incoming-call-avatar" id="lxst-incoming-call-avatar"></div>' +
                    '<div class="lxst-incoming-call-peer-meta">' +
                        '<span class="lxst-incoming-call-label">Ringing</span>' +
                        '<span class="lxst-incoming-call-name" id="lxst-incoming-call-name"></span>' +
                        '<span class="lxst-incoming-call-address" id="lxst-incoming-call-address"></span>' +
                    '</div>' +
                '</div>' +
            '</div>' +
            '<div class="bottom-sheet-footer lxst-incoming-call-actions">' +
                '<button class="lxst-call-action lxst-incoming-call-reject" id="lxst-incoming-call-reject" type="button" title="Reject call" aria-label="Reject call">' + _voiceIcon('phone-off', 18) + '<span>Reject</span></button>' +
                '<button class="lxst-call-action lxst-call-action-answer" id="lxst-incoming-call-answer" type="button" title="Answer call" aria-label="Answer call">' + _voiceIcon('phone-incoming', 18) + '<span>Answer</span></button>' +
            '</div>';
        document.body.appendChild(existing);
        document.body.appendChild(sheet);
        var answer = document.getElementById('lxst-incoming-call-answer');
        var reject = document.getElementById('lxst-incoming-call-reject');
        if (answer) answer.addEventListener('click', _voiceAnswerCall);
        if (reject) reject.addEventListener('click', _voiceRejectCall);
        sheet.addEventListener('keydown', function(e) {
            if (e.key === 'Escape') {
                e.stopPropagation();
                _voiceRejectCall();
            }
        });
    }
    var avatar = document.getElementById('lxst-incoming-call-avatar');
    var name = document.getElementById('lxst-incoming-call-name');
    var address = document.getElementById('lxst-incoming-call-address');
    var peerInfo = _voicePeerDisplayInfo(incoming);
    if (avatar && typeof identityAvatar === 'function') {
        avatar.innerHTML = identityAvatar(peerInfo.avatarHash || incoming.remote_identity, 44);
    }
    if (name) name.textContent = peerInfo.name;
    if (address) {
        address.textContent = peerInfo.address || incoming.remote_identity || '';
        address.title = address.textContent;
    }
    if (sheet && typeof sheet.focus === 'function') {
        sheet.setAttribute('tabindex', '-1');
    }
}

function _voiceHandleUpdate(data) {
    if (!data || typeof data !== 'object') return;
    var previousActive = lxstVoiceState.active;
    var shouldPlayNoAnswerCue = false;
    if (data.type === 'service') {
        lxstVoiceState.available = true;
        lxstVoiceState.running = !!data.running;
        if (!lxstVoiceState.running) {
            lxstVoiceState.audioRunning = false;
            lxstVoiceState.audioMicrophone = false;
            lxstVoiceState.audioSpeaker = false;
            _voiceReleaseNativeCallRoutePrime();
            _voiceResetCallControls();
        }
    } else if (data.type === 'incoming') {
        if (!_voiceIncomingIsExact(data.link_id)) {
            lxstVoiceState.incoming = {
                link_id: data.link_id,
                remote_identity: data.remote_identity,
                remote_lxmf_destination: data.remote_lxmf_destination || null,
                status: 'ringing'
            };
        }
    } else if (data.type === 'outgoing_pending') {
        lxstVoiceState.active = {
            link_id: data.link_id || null,
            remote_identity: data.remote_identity,
            remote_lxmf_destination: data.remote_lxmf_destination || null,
            role: 'outgoing',
            status: 'calling'
        };
        lxstVoiceState.incoming = null;
        lxstVoiceState.lastError = null;
        _voiceTrackEstablished(lxstVoiceState.active);
    } else if (data.type === 'outgoing') {
        lxstVoiceState.active = {
            link_id: data.link_id,
            remote_identity: data.remote_identity,
            remote_lxmf_destination: data.remote_lxmf_destination || null,
            role: 'outgoing',
            status: 'calling'
        };
        _voiceTrackEstablished(lxstVoiceState.active);
    } else if (data.type === 'snapshot') {
        var snapshotCall = data.active_call || null;
        if (snapshotCall && snapshotCall.role === 'incoming' && snapshotCall.status !== 'established') {
            lxstVoiceState.incoming = snapshotCall;
            lxstVoiceState.active = null;
            _voiceTrackEstablished(null);
        } else {
            lxstVoiceState.active = snapshotCall;
            if (snapshotCall && snapshotCall.status === 'established') {
                lxstVoiceState.incoming = null;
            }
            if (!snapshotCall) {
                lxstVoiceState.incoming = null;
            }
            _voiceTrackEstablished(lxstVoiceState.active);
        }
        lxstVoiceState.audioRunning = !!(data.audio && data.audio.running);
        lxstVoiceState.audioMicrophone = !!(data.audio && data.audio.microphone);
        lxstVoiceState.audioSpeaker = !!(data.audio && data.audio.speaker);
        if (data.audio && typeof data.audio.microphone_muted === 'boolean') {
            lxstVoiceState.microphoneMuted = data.audio.microphone_muted;
        }
    } else if (data.type === 'outgoing_failed') {
        var failedMessage = data.message || 'Call could not be connected';
        lxstVoiceState.active = null;
        lxstVoiceState.incoming = null;
        lxstVoiceState.audioRunning = false;
        lxstVoiceState.audioMicrophone = false;
        lxstVoiceState.audioSpeaker = false;
        _voiceReleaseNativeCallRoutePrime();
        _voiceResetCallControls();
        lxstVoiceState.lastAudioWarningKey = null;
        lxstVoiceState.lastDialHash = null;
        lxstVoiceState.establishedAtMs = null;
        lxstVoiceState.establishedLinkId = null;
        _voiceTrackEstablished(null);
        if (failedMessage !== 'cancelled') {
            lxstVoiceState.lastError = failedMessage;
            _voiceNotify(failedMessage);
        }
    } else if (data.type === 'audio') {
        lxstVoiceState.audioRunning = !!data.running;
        lxstVoiceState.audioMicrophone = !!data.microphone;
        lxstVoiceState.audioSpeaker = !!data.speaker;
        if (typeof data.microphone_muted === 'boolean') {
            lxstVoiceState.microphoneMuted = data.microphone_muted;
        }
        if (lxstVoiceState.audioMicrophone && lxstVoiceState.audioSpeaker) {
            lxstVoiceState.lastAudioWarningKey = null;
        }
        if (Array.isArray(data.warnings) && data.warnings.length) {
            var warningKey = data.warnings.join('|');
            if (warningKey !== lxstVoiceState.lastAudioWarningKey) {
                lxstVoiceState.lastAudioWarningKey = warningKey;
                window.RS.diag('warn', '[lxst] audio warning:', data.warnings);
                if (!lxstVoiceState.audioMicrophone && lxstVoiceState.audioSpeaker) {
                    _voiceNotify('Microphone unavailable; speaker audio active');
                } else if (lxstVoiceState.audioMicrophone && !lxstVoiceState.audioSpeaker) {
                    _voiceNotify('Speaker unavailable; microphone active');
                } else {
                    _voiceNotify('Voice audio warning');
                }
            }
        }
    } else if (data.type === 'audio_control') {
        if (typeof data.microphone_muted === 'boolean') {
            lxstVoiceState.microphoneMuted = data.microphone_muted;
        }
    } else if (data.type === 'terminated') {
        var terminatedMatches = (!data.link_id) ||
            (lxstVoiceState.active && lxstVoiceState.active.link_id === data.link_id) ||
            (lxstVoiceState.incoming && lxstVoiceState.incoming.link_id === data.link_id);
        if (!terminatedMatches) return;
        _voiceAnswerToken++;
        shouldPlayNoAnswerCue = !!(previousActive
            && previousActive.role === 'outgoing'
            && previousActive.status !== 'established'
            && !data.reason
            && Date.now() > _voiceSuppressNoAnswerCueUntil);
        lxstVoiceState.active = null;
        lxstVoiceState.incoming = null;
        lxstVoiceState.audioRunning = false;
        lxstVoiceState.audioMicrophone = false;
        lxstVoiceState.audioSpeaker = false;
        _voiceReleaseNativeCallRoutePrime();
        _voiceResetCallControls();
        lxstVoiceState.lastAudioWarningKey = null;
        lxstVoiceState.lastDialHash = null;
        lxstVoiceState.establishedAtMs = null;
        lxstVoiceState.establishedLinkId = null;
        _voiceTrackEstablished(null);
    } else if (data.type === 'error') {
        lxstVoiceState.lastError = data.message || 'Voice call error';
        _voiceNotify(lxstVoiceState.lastError);
    }
    renderVoiceUi();
    if (shouldPlayNoAnswerCue) _voicePlayNoAnswerCue();
}

function normalizeContactRecord(c) {
    if (!c || typeof c !== 'object') return null;
    var hash = c.hash || c.dest_hash || '';
    if (hash === null || hash === undefined) hash = '';
    hash = _canonicalConversationHash(hash);
    if (!hash) return null;
    var services = Array.isArray(c.services) ? c.services.slice() : [];
    var caps = Array.isArray(c.caps) ? c.caps.slice() : [];
    return {
        hash: hash,
        display_name: c.display_name || '',
        trust: c.trust || '',
        notes: c.notes || '',
        first_seen: c.first_seen,
        last_seen: c.last_seen,
        services: services,
        supports_ratspeak: services.indexOf('ratspeak.client') !== -1,
        // Unknown kinds read as person; the raw value is kept.
        kind: c.kind || 'person',
        caps: caps,
        is_rsnode: c.kind === 'device' && caps.indexOf('rsnode') !== -1,
        // Its card says it takes updates; an rsNode without that cap proves it with a fresh status.
        can_update: c.kind === 'device' && (caps.indexOf('rsnode.update') !== -1 || caps.indexOf('rsnode') !== -1)
    };
}

function normalizeContactList(data) {
    if (!Array.isArray(data)) return [];
    var contacts = [];
    data.forEach(function(c) {
        var normalized = normalizeContactRecord(c);
        if (normalized) contacts.push(normalized);
    });
    return contacts;
}

function _lxmfRenderedImageKey(img) {
    var bubble = img.closest('.msg-bubble[data-msg-id]');
    if (!bubble) return null;
    return JSON.stringify([bubble.getAttribute('data-msg-id'),
        img.getAttribute('data-stored-name') || img.getAttribute('src')]);
}

function _captureLxmfRenderedImages(container) {
    var images = new Map();
    container.querySelectorAll('img.msg-img').forEach(function(img) {
        if (!img.complete || !img.naturalWidth) return;
        var name = img.getAttribute('data-stored-name');
        // Respect cache retirement/memory pressure; do not keep an evicted URL
        // alive just because the previous render still displays it.
        if (name && _getImageBlobUrl(name) !== img.getAttribute('src')) return;
        var key = _lxmfRenderedImageKey(img);
        if (key) images.set(key, img);
    });
    return images;
}

function _restoreLxmfRenderedImages(container, images) {
    container.querySelectorAll('img.msg-img').forEach(function(img) {
        var key = _lxmfRenderedImageKey(img);
        var previous = key && images.get(key);
        if (!previous) return;
        images.delete(key);
        img.replaceWith(previous);
    });
}

// Non-alphabetic first chars bucket into '#' (sorted after Z).
function contactSectionLetter(c) {
    var firstChar = ((c && c.display_name) || '').trim().charAt(0);
    return /^[A-Za-z]$/.test(firstChar) ? firstChar.toUpperCase() : '#';
}

function _lxmfPeers() {
    return (typeof PeersCache !== 'undefined' && PeersCache) ? PeersCache.enriched() : [];
}

function _peerInfo(hash) {
    var peers = _lxmfPeers();
    for (var i = 0; i < peers.length; i++) {
        if (peers[i].hash === hash) return peers[i];
    }
    return null;
}

// The chat header's status line: the Contacts sentence, with the route on desktop.
function _peerHeaderStatusHtml(peer) {
    if (!peer || !peer.last_seen) return 'Not heard yet';
    var route = peer.route_state === 'direct' || peer.route_state === 'routed' ? peer.route_label : '';
    return escapeHtml(RS.kit.time.lastHeard(peer.last_seen)) +
        (route ? '<span class="ui-desk-only"> · ' + escapeHtml(route) + '</span>' : '');
}

// Avatar 40 with presence: active or seen recently; nothing for older.
function _chatHeaderAvatarHtml(hash, peer) {
    var tier = peer && peer.last_seen ? RS.kit.presenceFor(peer.last_seen) : 'older';
    var html = RS.kit.avatar(hash, 40, tier === 'older' ? null : tier, peer && peer.last_seen);
    return html.replace(/^<span class="ui-avatar ui-avatar--40">/, '').replace(/<\/span>$/, '');
}

function _applyChatHeaderPresence() {
    if (!lxmfActiveContact) return;
    var peer = _peerInfo(lxmfActiveContact);
    var statusEl = document.getElementById('lxmf-chat-header-status');
    if (statusEl) {
        var statusHtml = _peerHeaderStatusHtml(peer);
        if (statusEl.innerHTML !== statusHtml) statusEl.innerHTML = statusHtml;
    }
    var avatarEl = document.getElementById('lxmf-contact-avatar');
    if (avatarEl) {
        var avatarHtml = _chatHeaderAvatarHtml(lxmfActiveContact, peer);
        if (avatarEl._rsAvatar !== avatarHtml) {
            avatarEl._rsAvatar = avatarHtml;
            avatarEl.innerHTML = avatarHtml;
        }
    }
}

function _lookupContactRecord(hash) {
    for (var i = 0; i < lxmfContacts.length; i++) {
        if (lxmfContacts[i].hash === hash) return lxmfContacts[i];
    }
    return null;
}

function _hashFallbackName(hash) {
    if (!hash) return 'Unknown';
    return typeof shortHash === 'function'
        ? shortHash(hash, 8, 4)
        : (hash.length > 16 ? hash.substring(0, 8) + '\u2026' + hash.substring(hash.length - 4) : hash);
}

function _conversationNameInfo(hash, payloadName, isContact) {
    var name = typeof payloadName === 'string' ? payloadName.trim() : '';
    if (name) return { name: name, isHash: false };

    var contact = _lookupContactRecord(hash);
    if (contact && contact.display_name) {
        return { name: contact.display_name, isHash: false };
    }

    var announceName = _lookupAnnounceName(hash);
    if (announceName) return { name: announceName, isHash: false };

    if (isContact || contact) return { name: 'Anonymous', isHash: false };
    return { name: _hashFallbackName(hash), isHash: true };
}

function _conversationPayloadForHash(hash) {
    if (!Array.isArray(lxmfConversations)) return null;
    for (var i = 0; i < lxmfConversations.length; i++) {
        if (lxmfConversations[i] && lxmfConversations[i].hash === hash) return lxmfConversations[i];
    }
    return null;
}

function _refreshRenderedConversationNames() {
    document.querySelectorAll('.msg-conv-row[data-hash]').forEach(function(row) {
        var hash = row.dataset.hash;
        var titleEl = row.querySelector('.ui-row-title');
        if (!hash || !titleEl) return;
        var payload = _conversationPayloadForHash(hash);
        var info = _conversationNameInfo(
            hash,
            payload ? payload.display_name : null,
            payload ? payload.is_contact : false
        );
        var html = info.isHash
            ? '<span class="msg-title-hash" title="' + escapeHtml(RS.kit.hash.text(hash, 'full')) + '">' + escapeHtml(info.name) + '</span>'
            : ratspeakDisplayNameHtml(info.name, hash);
        if (titleEl.innerHTML !== html) titleEl.innerHTML = html;
    });

    if (lxmfActiveContact) _renderChatHeaderName();
}

function _refreshRenderedConversationPresence() {
    document.querySelectorAll('.msg-conv-row[data-hash]').forEach(function(row) {
        var hash = row.dataset.hash;
        var avatar = row.querySelector('.ui-avatar');
        if (!hash || !avatar) return;
        var presence = _conversationPresence(hash);
        var key = presence.active ? 'active' : '';
        if ((row.getAttribute('data-presence') || '') === key) return;
        row.setAttribute('data-presence', key);
        avatar.outerHTML = RS.kit.avatar(hash, 'row', presence.active ? 'active' : null, presence.heard);
    });
    _applyChatHeaderPresence();
}

function _peerCompactIfaceLabel(iface) {
    if (!iface) return null;
    var s = String(iface).toLowerCase();
    if (s.indexOf('ble_peer') >= 0 || s.indexOf('ble mesh') >= 0 || s.indexOf('bluetooth peer') >= 0) return 'BLE';
    return null;
}

function _deliveryPrefOrAuto(method) {
    return method || 'auto';
}

function _optimisticDeliveryMethod(method) {
    return method === 'auto' ? null : method;
}

function _utf8ByteLength(value) {
    if (typeof RS !== 'undefined' && RS.text && typeof RS.text.utf8Length === 'function') {
        return RS.text.utf8Length(value);
    }
    return new Blob([value || '']).size;
}

var RS_LINK_TLDS = [
    'com', 'org', 'net', 'io', 'fi', 'app', 'dev', 'ai', 'co', 'us', 'uk',
    'ca', 'de', 'fr', 'nl', 'se', 'no', 'dk', 'ch', 'au', 'jp', 'me',
    'info', 'biz', 'xyz', 'online', 'site', 'tech', 'cloud', 'network',
    'systems', 'software', 'chat', 'social', 'news', 'live', 'store',
    'blog', 'wiki', 'earth', 'world', 'one'
];
var RS_LINK_RE = new RegExp(
    "(https?:\\/\\/[^\\s<>\"']+)|((?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\\.)+(?:" +
    RS_LINK_TLDS.join('|') +
    ")(?:\\/[^\\s<>\"']*)?)",
    'ig'
);

function _splitTrailingUrlPunctuation(raw) {
    var url = raw || '';
    var trailing = '';
    while (url.length && /[.,!?;:]$/.test(url)) {
        trailing = url.slice(-1) + trailing;
        url = url.slice(0, -1);
    }
    while (url.length && /[\])]$/.test(url)) {
        var close = url.slice(-1);
        var open = close === ')' ? '(' : '[';
        if (url.split(close).length <= url.split(open).length) break;
        trailing = close + trailing;
        url = url.slice(0, -1);
    }
    return { url: url, trailing: trailing };
}

var RS_PAPER_LINK_RE = /lxm:\/\/[A-Za-z0-9_-]+/ig;
var RS_QR_GLYPH_PATHS = '<rect x="3" y="3" width="7" height="7"/><rect x="14" y="3" width="7" height="7"/><rect x="3" y="14" width="7" height="7"/><path d="M14 14h3v3h-3z"/><path d="M19 14h2"/><path d="M14 21h7v-2"/><path d="M19 17h2"/>';
var RS_PAPER_LINK_GLYPH = '<svg class="rs-paper-link-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">' + RS_QR_GLYPH_PATHS + '</svg>';

// The href is regex-limited to base64url, so it needs no escaping.
function _paperLinkHtml(href) {
    return '<a class="rs-message-link rs-paper-link" href="#" data-url="' + href +
        '" rel="noopener noreferrer">' + RS_PAPER_LINK_GLYPH + '<span>Paper message</span></a>';
}

function _messageLinkRanges(text) {
    var value = String(text || '');
    var ranges = [];
    var paper = [];
    value.replace(RS_PAPER_LINK_RE, function(match, offset) {
        if (offset > 0 && /[A-Za-z0-9]/.test(value.charAt(offset - 1))) return match;
        paper.push({ start: offset, end: offset + match.length, href: match, paper: true, html: _paperLinkHtml(match) });
        return match;
    });
    value.replace(RS_LINK_RE, function(match, schemeUrl, bareUrl, offset) {
        var prev = offset > 0 ? value.charAt(offset - 1) : '';
        if (prev === '@') return match;
        var label = _splitTrailingUrlPunctuation(match).url;
        if (!label) return match;
        var end = offset + label.length;
        if (paper.some(function(p) { return offset < p.end && end > p.start; })) return match;
        ranges.push({
            start: offset,
            end: end,
            href: /^https?:\/\//i.test(label) ? label : 'https://' + label,
        });
        return match;
    });
    if (!paper.length) return ranges;
    return ranges.concat(paper).sort(function(a, b) { return a.start - b.start; });
}

function linkifyMessageText(text) {
    var value = String(text || '');
    if (!value) return '';
    var out = '';
    var last = 0;
    _messageLinkRanges(value).forEach(function(range) {
        var label = value.slice(range.start, range.end);
        out += escapeHtml(value.slice(last, range.start));
        if (range.html) {
            out += range.html;
            last = range.end;
            return;
        }
        out += '<a class="rs-message-link" href="' + escapeHtml(range.href) +
            '" data-url="' + escapeHtml(range.href) + '" rel="noopener noreferrer">' +
            escapeHtml(label) + '</a>';
        last = range.end;
    });
    out += escapeHtml(value.slice(last));
    return out.replace(/\n/g, '<br>');
}

function _messageIsMarkdown(msg) {
    return !!(msg && Number(msg.renderer) === 2 && window.RS && RS.markdown);
}

function _stripMessageMarkdown(text) {
    return window.RS && RS.markdown ? RS.markdown.strip(text) : String(text || '');
}

function _messageBodyHtml(msg, hasRenderedAudio) {
    var body = _messageBodyContent(msg, hasRenderedAudio);
    var title = ((msg && msg.title) || '').trim();
    if (_messageIsMarkdown(msg) && body.trim()) {
        var rendered = RS.markdown.render(body, { links: _messageLinkRanges });
        if (rendered != null) {
            return { html: (title ? '<p class="md-p">' + escapeHtml(title) + '</p>' : '') + rendered, markdown: true };
        }
    }
    var text = _messageDisplayContent(msg, hasRenderedAudio);
    return { html: text ? linkifyMessageText(text) : '', markdown: false };
}

// Delivery marks on a 16 px grid, one colour except Not delivered. One check is Sent,
// two are Delivered (the recipient's proof); the envelope is an Offline Inbox deposit.
var MSG_MARKS = {
    sending: ['Sending', '<circle cx="8" cy="8" r="6"/><polyline points="8 4.5 8 8 10.5 9.5"/>'],
    sent: ['Sent', '<polyline points="3 8.5 6.5 12 13 4.5"/>'],
    delivered: ['Delivered', '<polyline points="1.5 8.5 5 12 11.5 4.5"/><polyline points="8.5 11 9.5 12 16 4.5"/>'],
    propagated: ['In the Offline Inbox', '<rect x="2" y="3.5" width="12" height="9" rx="1.5"/><polyline points="2.5 4.5 8 9 13.5 4.5"/>'],
    waiting: ['Waiting to send', '<path d="M4 2h8M4 14h8M5 2v2.5L8 8l-3 3.5V14M11 2v2.5L8 8l3 3.5V14"/>'],
    failed: ['Not delivered', '<line x1="8" y1="2.5" x2="8" y2="9.5"/><line x1="8" y1="13" x2="8" y2="13.01"/>'],
    cancelled: ['Sending cancelled', '<line x1="4" y1="4" x2="12" y2="12"/><line x1="12" y1="4" x2="4" y2="12"/>'],
    rejected: ['Rejected', '<circle cx="8" cy="8" r="6"/><line x1="5" y1="5" x2="11" y2="11"/><line x1="11" y1="5" x2="5" y2="11"/>'],
    paper: ['Paper message', '<rect x="2" y="2" width="5" height="5"/><rect x="9" y="2" width="5" height="5"/><rect x="2" y="9" width="5" height="5"/><path d="M9 9h2v2H9zM13 9v1M9 14h5v-2"/>']
};

var MSG_WAITING_TITLE = 'Waiting to send. Ratspeak keeps trying for up to 24 hours.';

function _messageMarkSvg(kind) {
    return '<svg class="msg-mark-ico" viewBox="0 0 16 16" aria-hidden="true">' + MSG_MARKS[kind][1] + '</svg>';
}

function _messageMarkKind(state) {
    if (state === 'paper') return 'paper';
    // "!" as other messengers mark a send that needs you; the cross is for what you cancelled.
    if (state === 'failed' || state === 'timeout') return 'failed';
    // Parked until it can go; the clock is for an attempt that is running now.
    if (['cancelled', 'rejected', 'propagated', 'delivered', 'sent', 'waiting'].indexOf(state) !== -1) return state;
    // Anything else is on its way: queued, routing, Link setup, or Resource transfer.
    return 'sending';
}

// Not delivered and Waiting are buttons: they open the message menu (Retry, Try now, Activity).
function _messageStateIconHtml(msg) {
    var kind = _messageMarkKind(msg.state);
    var label = MSG_MARKS[kind][0];
    if (kind === 'failed' || kind === 'waiting') {
        var title = kind === 'waiting' ? MSG_WAITING_TITLE : label;
        return '<button type="button" class="msg-mark msg-mark--' + kind + '" data-msg-id="' + escapeHtml(msg.id || '') + '" ' +
            'aria-haspopup="dialog" aria-expanded="false" aria-label="' + label + '. Options" title="' + title + '">' + _messageMarkSvg(kind) + '</button>';
    }
    if (kind === 'sending') {
        var progressPercent = _messageProgressPercent(msg);
        if (progressPercent !== null) label += ' ' + progressPercent + '%';
    }
    return '<span class="msg-mark msg-mark--' + kind + '" role="img" aria-label="' + label + '" title="' + label + '">' +
        _messageMarkSvg(kind) + (kind === 'rejected' ? '<span>Rejected</span>' : '') + '</span>';
}

function _isCanonicalLxmfMsgId(msgId) {
    return /^[a-f0-9]{64}$/i.test(String(msgId || ''));
}

function _messageDeliveryMethod(msg) {
    return String((msg && msg.delivery_method) || 'opportunistic').toLowerCase();
}

function _messageIsPaper(msg) {
    return !!msg && (msg.state === 'paper' || _messageDeliveryMethod(msg) === 'paper');
}

// Local notes (rsNode update outcomes) are never sent and never alert.
function _messageIsNote(msg) {
    return !!msg && msg.direction === 'note';
}

// Paper never touches the network, so "Paper" replaces the delivery mark; yours reopens the QR.
function _messagePaperChipHtml(msg, isOut) {
    var glyph = _messageMarkSvg('paper');
    if (isOut) {
        return '<button type="button" class="msg-mark msg-mark--paper" data-msg-id="' + escapeHtml(msg.id || '') + '" ' +
            'aria-label="Paper message, not sent over the network. Show QR code" title="Show QR code">' + glyph +
            '<span aria-hidden="true">Paper</span></button>';
    }
    return '<span class="msg-mark msg-mark--paper" role="img" aria-label="Received as a paper message">' +
        glyph + '<span aria-hidden="true">Paper</span></span>';
}

function _messageCanCancelSend(msg) {
    if (!msg || msg.direction !== 'outbound') return false;
    if (msg.state === 'sent') return _messageDeliveryMethod(msg) === 'direct';
    return _isUnsettledMessageState(msg.state);
}

// Retry sends the stored message again; rows from before the outbox have none.
function _messageCanRetry(msg) {
    return !!msg && msg.direction === 'outbound' && msg.retryable !== false &&
        ['failed', 'timeout', 'cancelled'].indexOf(msg.state) !== -1 &&
        _isCanonicalLxmfMsgId(msg.id) && !_messageIsPaper(msg);
}

// Activity explains a send that waits or ended undelivered; cancelling explains itself.
function _messageCanShowInActivity(msg) {
    return !!msg && msg.direction === 'outbound' &&
        ['waiting', 'failed', 'timeout', 'rejected'].indexOf(msg.state) !== -1 &&
        _isCanonicalLxmfMsgId(msg.id) && !_messageIsPaper(msg);
}

// Try now and Stop trying act on the stored message, which only its final id names.
function _messageIsWaiting(msg) {
    return !!msg && msg.direction === 'outbound' && msg.state === 'waiting' &&
        _isCanonicalLxmfMsgId(msg.id);
}

function _offlineInboxAvailable() {
    if (typeof propagationStatus === 'undefined' || propagationStatus.mode === 'off') return false;
    return propagationStatus.mode === 'auto'
        ? !!propagationStatus.auto_active_node
        : !!propagationStatus.propagation_node;
}

function _retryLxmfMessage(msg, method) {
    return RS.invoke('retry_lxmf_message', {
        args: { msg_id: msg.id, delivery_method: method }
    }).then(function(resp) {
        var current = _findLxmfMessageById(msg.id) || msg;
        current.state = 'sending';
        if (resp && resp.method) current.delivery_method = resp.method;
        delete current.rtt;
        delete current.delivery_progress;
        _cacheActiveConversation();
        renderConversation();
        loadConversations();
    }).catch(function(err) {
        showToast((err && err.message) || 'Could not send this message again', 'toast-error', 3500);
    });
}

// Retry… : how to send this message again.
function _openRetryMethodPopover(msg, anchor) {
    var container = document.getElementById('lxmf-messages');
    // The bubble, so the list opens beside the message rather than mid-row.
    anchor = anchor && anchor.isConnected ? anchor : _findRenderedMessageBubble(container, msg.id);
    if (!anchor) {
        _retryLxmfMessage(msg, 'auto');
        return;
    }
    RS.kit.menu(anchor, { title: 'Send by', caption: 'Send this message by', hints: true, align: msg.direction === 'outbound' ? 'end' : 'start', items: [
        { label: 'Automatic', icon: 'refresh', onSelect: function() { _retryLxmfMessage(msg, 'auto'); } },
        { label: 'Opportunistic', icon: 'opportunistic', hint: 'Single packets. Best for short text.', onSelect: function() { _retryLxmfMessage(msg, 'opportunistic'); } },
        { label: 'Direct', icon: 'direct', hint: 'Opens a link first. Best for long text and files.', onSelect: function() { _retryLxmfMessage(msg, 'direct'); } },
        { label: 'Offline Inbox', icon: 'envelope', disabled: !_offlineInboxAvailable(),
            hint: _offlineInboxAvailable() ? 'Held by an Offline Inbox node until they collect it.' : 'Turn on Offline Inbox in Settings to use it.',
            onSelect: function() { _retryLxmfMessage(msg, 'propagated'); } },
    ] });
}

function _messageHasTransferPayload(msg) {
    return !!(msg && (msg.image || (Array.isArray(msg.attachments) && msg.attachments.length > 0)));
}

function _messageCanCancelTransfer(msg) {
    return _messageHasTransferPayload(msg) && _messageCanCancelSend(msg);
}

function _messageTransferPayloadSize(msg) {
    if (!msg) return 0;
    var total = 0;
    if (msg.image && typeof msg.image.size === 'number' && isFinite(msg.image.size)) {
        total += Math.max(0, msg.image.size);
    }
    if (Array.isArray(msg.attachments)) {
        for (var i = 0; i < msg.attachments.length; i++) {
            var attachment = msg.attachments[i] || {};
            if (typeof attachment.size === 'number' && isFinite(attachment.size)) {
                total += Math.max(0, attachment.size);
            }
        }
    }
    return total;
}

function _messageShowsTransferPercent(msg) {
    if (!_messageCanCancelTransfer(msg)) return false;
    var efficientBytes = lxmfLimits.efficient_resource_bytes || 1048575;
    return _messageTransferPayloadSize(msg) > efficientBytes;
}

function _messageProgressPercent(msg) {
    if (!_messageShowsTransferPercent(msg)) return null;
    var progress = msg && typeof msg.delivery_progress === 'number' ? msg.delivery_progress : null;
    if (progress === null || !isFinite(progress) || progress <= 0 || progress >= 1) return null;
    // LXMF reserves the first 10% for setup. Large attachment meters show
    // the Resource payload phase, with 1% while starting and at most 99%
    // until the receiver's proof confirms delivery.
    var payloadProgress = Math.max(0, (progress - 0.10) / 0.90);
    return Math.max(1, Math.min(99, Math.round(payloadProgress * 100)));
}

function _messageProgressMetaHtml(msg) {
    var percent = _messageProgressPercent(msg);
    return percent === null ? '' : '<span class="msg-progress">' + percent + '%</span>';
}

// An image on its way: a spinner, the percent, and cancel on the picture.
function _messageSendCancelOverlayHtml(msg, percent) {
    return '<span class="msg-image-progress"><span class="ui-spin" aria-hidden="true"></span>' +
        (percent === null ? '' : '<span>' + percent + '%</span>') +
        '<button type="button" class="ui-icon-btn ui-icon-btn--sm ui-icon-btn--media msg-cancel" ' +
        'data-msg-id="' + escapeHtml(msg.id || '') + '" aria-label="Cancel sending message" title="Cancel sending">' +
        RS.kit.icon('x') + '</button></span>';
}

function _messageInlineCancelHtml(msg) {
    if (!_messageCanCancelSend(msg)) return '';
    return '<button type="button" class="msg-cancel msg-cancel--text" ' +
        'data-msg-id="' + escapeHtml(msg.id || '') + '" aria-label="Cancel sending message">Cancel</button>';
}

function _findLxmfMessageById(msgId) {
    for (var i = 0; i < lxmfConversation.length; i++) {
        if (lxmfConversation[i].id === msgId) return lxmfConversation[i];
    }
    return null;
}

function _markLxmfMessageCancelled(msgId) {
    var msg = _findLxmfMessageById(msgId);
    if (!msg) return;
    msg.state = 'cancelled';
    delete msg.delivery_progress;
    delete msg.delivery_link_id;
    delete msg.delivery_representation;
    _cacheActiveConversation();
    renderConversation();
}

function _invokeLxmfCancel(msgId) {
    return RS.invoke('cancel_lxmf_message', {
        args: { msg_id: msgId }
    });
}

function _showLxmfStopResult(resp) {
    if (!resp) return;
    if (resp.live_owner_stopped) {
        showToast('Cancelled local retries. A copy already handed to the network may still arrive.', 'toast-info', 5200);
        return;
    }
    if (resp.cancelled && resp.row_marked_stopped) {
        showToast('No live send remained. The local message was cancelled, but a copy may still arrive.', 'toast-info', 5200);
        return;
    }
    if (resp.cancelled && !resp.may_have_left_device) {
        showToast('Cancelled before the message left this device.', 'toast-info', 3200);
        return;
    }
    if (resp.may_have_left_device) {
        showToast('No local send remained to cancel. A copy may already have left this device.', 'toast-info', 5200);
    }
}

function _flushPendingLxmfCancel(clientMsgId, serverMsgId) {
    if (!clientMsgId || !serverMsgId || !_pendingLxmfCancelByClientId[clientMsgId]) return;
    delete _pendingLxmfCancelByClientId[clientMsgId];
    _invokeLxmfCancel(serverMsgId).then(function(resp) {
        if (resp && resp.cancelled) {
            _markLxmfMessageCancelled(serverMsgId);
        }
        _showLxmfStopResult(resp);
    }).catch(function(err) {
        showToast('Could not stop sending: ' + ((err && err.message) || 'error'), 'toast-error', 3500);
    });
}

function _cancelLxmfSend(msgId) {
    msgId = String(msgId || '');
    if (!msgId) return;
    rsConfirm({
        title: 'Cancel sending?',
        message: 'Cancel preparation and retries for this message?',
        confirmText: 'Cancel sending'
    }).then(function(confirmed) {
        if (!confirmed) return;
        if (!_isCanonicalLxmfMsgId(msgId)) {
            _pendingLxmfCancelByClientId[msgId] = true;
        }
        return _invokeLxmfCancel(msgId).then(function(resp) {
            if (resp && resp.cancelled) {
                _markLxmfMessageCancelled(resp.msg_id || resp.client_msg_id || msgId);
            }
            _showLxmfStopResult(resp);
        });
    }).catch(function(err) {
        showToast('Could not stop sending: ' + ((err && err.message) || 'error'), 'toast-error', 3500);
    });
}

// The attempt's own `sending` step moves the bubble; a wake that is held back leaves it waiting.
function _tryWaitingLxmfMessage(msg) {
    return RS.invoke('try_waiting_now', {
        args: { msg_id: msg.id }
    }).catch(function(err) {
        showToast((err && err.message) || 'Could not send this message again', 'toast-error', 3500);
    });
}

// Opening the menu is the deliberate step and Retry undoes it, so this asks nothing.
function _stopWaitingLxmfMessage(msg) {
    return _invokeLxmfCancel(msg.id).then(function(resp) {
        if (resp && resp.cancelled) {
            _markLxmfMessageCancelled(resp.msg_id || msg.id);
        }
        // A message that only waited had nothing in the air: the mark says it all.
        if (!(resp && resp.only_waiting)) _showLxmfStopResult(resp);
    }).catch(function(err) {
        showToast('Could not stop sending: ' + ((err && err.message) || 'error'), 'toast-error', 3500);
    });
}

function _conversationMessagesFor(hash, thread) {
    hash = _canonicalConversationHash(hash);
    thread = _resolveConversationThread(hash, thread);
    var key = _conversationKey(hash, thread);
    if (hash && hash === _canonicalConversationHash(lxmfActiveContact) && thread === lxmfActiveThread) {
        return { messages: lxmfConversation, active: true, key: key };
    }
    return { messages: (cacheGet(key) || []).slice(), active: false, key: key };
}

function _commitConversationMessages(hash, target) {
    var key = target.key || _canonicalConversationHash(hash);
    if (target.active && key === _activeConversationKey()) {
        lxmfConversation = target.messages;
        _cacheActiveConversation();
        return;
    }
    if (key) cacheSet(key, target.messages);
}

function _appendConversationMessage(hash, message) {
    var target = _conversationMessagesFor(hash, message && message.thread);
    target.messages.push(message);
    _commitConversationMessages(hash, target);
    return target.active;
}

function _handleLxmfSendAccepted(resp, clientMsgId, targetHash) {
    var owner = arguments.length > 3 ? arguments[3] : null;
    if (owner && !_conversationOwnerIdentityIsCurrent(owner)) return;
    var serverMsgId = resp && resp.msg_id;
    if (!clientMsgId) return;
    if (!serverMsgId) {
        if (resp && resp.cancelled) delete _pendingLxmfCancelByClientId[clientMsgId];
        return;
    }
    var target = _conversationMessagesFor(targetHash || lxmfActiveContact, owner ? owner.thread : undefined);
    for (var i = 0; i < target.messages.length; i++) {
        if (target.messages[i].id === clientMsgId) {
            target.messages[i].id = serverMsgId;
            break;
        }
    }
    _commitConversationMessages(targetHash || lxmfActiveContact, target);
    if (resp && resp.cancelled) {
        delete _pendingLxmfCancelByClientId[clientMsgId];
    } else {
        _flushPendingLxmfCancel(clientMsgId, serverMsgId);
    }
}

function cacheGet(hash) {
    hash = _canonicalConversationHash(hash);
    var msgs = _conversationCache[hash];
    if (msgs) {
        var idx = _cacheLru.indexOf(hash);
        if (idx !== -1) { _cacheLru.splice(idx, 1); _cacheLru.push(hash); }
    }
    return msgs;
}
function cacheSet(hash, messages) {
    hash = _canonicalConversationHash(hash);
    if (!hash) return;
    var size = 0;
    try { size = _utf8ByteLength(JSON.stringify(messages || [])); }
    catch (_) { size = _conversationCacheMaxBytes + 1; }
    cacheDel(hash);
    if (size > _conversationCacheMaxBytes) return;
    var idx = _cacheLru.indexOf(hash);
    if (idx !== -1) _cacheLru.splice(idx, 1);
    _cacheLru.push(hash);
    _conversationCache[hash] = messages;
    _conversationCacheSizes[hash] = size;
    _conversationCacheBytes += size;
    while (_cacheLru.length > _cacheMax || _conversationCacheBytes > _conversationCacheMaxBytes) {
        var evict = _cacheLru.shift();
        _conversationCacheBytes = Math.max(0, _conversationCacheBytes - (_conversationCacheSizes[evict] || 0));
        delete _conversationCache[evict];
        delete _conversationCacheSizes[evict];
    }
}
function cacheDel(hash) {
    hash = _canonicalConversationHash(hash);
    _conversationCacheBytes = Math.max(0, _conversationCacheBytes - (_conversationCacheSizes[hash] || 0));
    delete _conversationCache[hash];
    delete _conversationCacheSizes[hash];
    var idx = _cacheLru.indexOf(hash);
    if (idx !== -1) _cacheLru.splice(idx, 1);
}

function _cacheActiveConversation() {
    if (lxmfActiveContact) cacheSet(_activeConversationKey(), lxmfConversation.slice());
}

// Forget every cached conversation with `hash`.
function _cacheDelContact(hash) {
    hash = _canonicalConversationHash(hash);
    _cacheLru.slice().forEach(function(key) {
        if (key === hash || key.indexOf(hash + '~') === 0) cacheDel(key);
    });
}

function _mergeConversationMessages(hash, serverMessages) {
    hash = _canonicalConversationHash(hash);
    var merged = Array.isArray(serverMessages) ? serverMessages.slice() : [];
    var cached = cacheGet(hash) || [];
    if (!cached.length) return merged;

    var seen = {};
    merged.forEach(function(msg) {
        if (msg && msg.id) seen[msg.id] = true;
    });

    var now = Date.now() / 1000;
    cached.forEach(function(msg) {
        if (!msg || !msg.id || seen[msg.id] || msg.direction !== 'outbound') return;
        var state = msg.state || '';
        var recent = !msg.timestamp || (now - msg.timestamp) < 3600;
        if (recent && (_isUnsettledMessageState(state) || state === 'propagated')) {
            merged.push(msg);
            seen[msg.id] = true;
        }
    });

    merged.sort(function(a, b) {
        var at = Number(a && a.timestamp) || 0;
        var bt = Number(b && b.timestamp) || 0;
        if (at === bt) return 0;
        return at < bt ? -1 : 1;
    });
    return merged;
}

function _imageBlobUrlBudget() {
    var mobile = (typeof isTauriMobile === 'function' && isTauriMobile()) ||
        (typeof isMobile === 'function' && isMobile());
    return mobile ? (16 * 1024 * 1024) : (32 * 1024 * 1024);
}

function _evictImageBlobUrl(name) {
    var entry = _imageBlobUrlCache[name];
    if (!entry) return;
    delete _imageBlobUrlCache[name];
    var idx = _imageBlobUrlLru.indexOf(name);
    if (idx !== -1) _imageBlobUrlLru.splice(idx, 1);
    _imageBlobUrlBytes = Math.max(0, _imageBlobUrlBytes - (entry.size || 0));
    if (entry.url) {
        try { URL.revokeObjectURL(entry.url); } catch (_) {}
    }
}

function _clearImageBlobUrlCache() {
    _imageCacheGeneration++;
    Object.keys(_imageBlobUrlCache).forEach(_evictImageBlobUrl);
    _imageBlobUrlCache = {};
    _imageBlobUrlLru = [];
    _imageBlobUrlBytes = 0;
    _imageHydrationQueue = [];
    if (_imageHydrationObserver) _imageHydrationObserver.disconnect();
}

function handleAttachmentMemoryPressure(critical) {
    _clearImageBlobUrlCache();
    if (critical) {
        _conversationCache = {};
        _conversationCacheSizes = {};
        _conversationCacheBytes = 0;
        _cacheLru = [];
        if (lxmfPendingFile) clearPendingFile();
    }
}

function _rememberImageBlobUrl(name, url, file) {
    var size = Number(file && file.size) || Number(file && file.blob && file.blob.size) || 0;
    var budget = _imageBlobUrlBudget();
    if (size > budget) return false;
    var existing = _imageBlobUrlCache[name];
    if (existing) _evictImageBlobUrl(name);
    var idx = _imageBlobUrlLru.indexOf(name);
    if (idx !== -1) _imageBlobUrlLru.splice(idx, 1);
    _imageBlobUrlLru.push(name);
    _imageBlobUrlCache[name] = {
        url: url,
        ts: Date.now(),
        size: size,
        file: file ? {
            url: url,
            blob: file.blob || null,
            size: size,
            filename: file.filename || name,
            mime: file.mime || 'application/octet-stream',
        } : null,
    };
    _imageBlobUrlBytes += size;
    while (_imageBlobUrlLru.length > _imageBlobUrlMax || _imageBlobUrlBytes > budget) {
        var evict = _imageBlobUrlLru.shift();
        _evictImageBlobUrl(evict);
    }
    return !!_imageBlobUrlCache[name];
}

function _getImageBlobUrl(name) {
    var entry = _imageBlobUrlCache[name];
    if (!entry) return null;
    var idx = _imageBlobUrlLru.indexOf(name);
    if (idx !== -1) {
        _imageBlobUrlLru.splice(idx, 1);
        _imageBlobUrlLru.push(name);
    }
    return entry.url;
}

function _getImageDownloadFile(name) {
    var entry = _imageBlobUrlCache[name];
    return entry && entry.file ? entry.file : null;
}

function _makeImageBlobUrlRoom(name, incomingBytes) {
    var budget = _imageBlobUrlBudget();
    if (incomingBytes > budget) return false;
    while (_imageBlobUrlBytes + incomingBytes > budget && _imageBlobUrlLru.length) {
        var evict = _imageBlobUrlLru[0];
        if (evict === name) {
            _imageBlobUrlLru.shift();
            _imageBlobUrlLru.push(evict);
            if (_imageBlobUrlLru.length === 1) return false;
            continue;
        }
        _evictImageBlobUrl(evict);
    }
    return _imageBlobUrlBytes + incomingBytes <= budget;
}

function _imageHydrationPromise(name) {
    if (_imageHydrationInFlight[name]) return _imageHydrationInFlight[name];
    var generation = _imageCacheGeneration;
    var promise = RS.fileMetadata(name).then(function(meta) {
        var size = Number(meta && meta.size) || 0;
        if (!meta || !meta.inline_image) {
            var unsafe = new Error('Image is available as a file');
            unsafe.code = 'inline_image_unsafe';
            throw unsafe;
        }
        if (size > _imageBlobUrlBudget()) {
            var large = new Error('Image is too large for inline display');
            large.code = 'inline_image_memory_limit';
            throw large;
        }
        if (!_makeImageBlobUrlRoom(name, size)) {
            var pressure = new Error('Image cache is full');
            pressure.code = 'inline_image_memory_limit';
            throw pressure;
        }
        return RS.fileDownload(name, meta);
    }).then(function(file) {
        if (generation !== _imageCacheGeneration) {
            try { URL.revokeObjectURL(file.url); } catch (_) {}
            var stale = new Error('Image request was superseded');
            stale.code = 'inline_image_stale';
            throw stale;
        }
        if (!_rememberImageBlobUrl(name, file.url, file)) {
            try { URL.revokeObjectURL(file.url); } catch (_) {}
            var pressure = new Error('Image cache is full');
            pressure.code = 'inline_image_memory_limit';
            throw pressure;
        }
        return _getImageDownloadFile(name);
    });
    var tracked = promise.finally(function() {
        if (_imageHydrationInFlight[name] === tracked) delete _imageHydrationInFlight[name];
    });
    _imageHydrationInFlight[name] = tracked;
    return tracked;
}

function _drainImageHydrationQueue() {
    while (_imageHydrationActive < _imageHydrationMax && _imageHydrationQueue.length) {
        var task = _imageHydrationQueue.shift();
        if (!task.img || !task.img.isConnected) continue;
        _imageHydrationActive++;
        (function(activeTask) {
            _imageHydrationPromise(activeTask.name).then(function(file) {
                var img = activeTask.img;
                if (!img.isConnected || img.getAttribute('data-stored-name') !== activeTask.name) return;
                if (file.filename) img.setAttribute('data-filename', file.filename);
                if (file.mime) img.setAttribute('data-mime', file.mime);
                var btn = img.closest('.msg-image-btn');
                if (btn) {
                    if (file.filename) btn.setAttribute('data-filename', file.filename);
                    if (file.mime) btn.setAttribute('data-mime', file.mime);
                }
                img.src = file.url;
            }).catch(function(err) {
                var img = activeTask.img;
                if (!img || !img.isConnected || (err && err.code === 'inline_image_stale')) return;
                img.classList.add('is-error');
                img.setAttribute('alt', (err && err.code === 'inline_image_unsafe')
                    ? 'Image available as a file'
                    : 'Image not loaded');
            }).finally(function() {
                _imageHydrationActive = Math.max(0, _imageHydrationActive - 1);
                _drainImageHydrationQueue();
            });
        })(task);
    }
}

function _queueImageHydration(img, name) {
    if (img.getAttribute('data-hydration-queued') === 'true') return;
    img.setAttribute('data-hydration-queued', 'true');
    _imageHydrationQueue.push({ img: img, name: name });
    _drainImageHydrationQueue();
}

function _observeImageHydration(img, name) {
    if (typeof IntersectionObserver !== 'function') {
        _queueImageHydration(img, name);
        return;
    }
    if (!_imageHydrationObserver) {
        _imageHydrationObserver = new IntersectionObserver(function(entries) {
            entries.forEach(function(entry) {
                if (!entry.isIntersecting) return;
                _imageHydrationObserver.unobserve(entry.target);
                _queueImageHydration(entry.target, entry.target.getAttribute('data-stored-name'));
            });
        }, { rootMargin: '400px 0px' });
    }
    _imageHydrationObserver.observe(img);
}

// Suppresses the synthetic click after swipe touchend.
var _convSwipedRecently = false;

function _addConvDeleteIndicator(row) {
    if (row.querySelector('.conv-swipe-delete')) return;
    var indicator = document.createElement('div');
    indicator.className = 'conv-swipe-delete';
    indicator.innerHTML = '<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="3 6 5 6 21 6"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/></svg>';
    row.style.position = 'relative';
    row.appendChild(indicator);
}

function _resetConvRowVisual(row) {
    row.style.transition = 'transform 0.2s ease';
    row.style.transform = '';
    var del = row.querySelector('.conv-swipe-delete');
    if (del) del.remove();
}

function _suppressNextConvClick() {
    _convSwipedRecently = true;
    setTimeout(function() { _convSwipedRecently = false; }, 400);
}

function _removeGhostRow() {
    if (!_ghostConversationHash) return;
    var container = document.getElementById('lxmf-conversations-list');
    if (container) {
        var ghost = container.querySelector('.msg-conv-row[data-ghost="true"]');
        if (ghost && ghost.parentNode) ghost.parentNode.remove();
    }
    _ghostConversationHash = null;
}

function _conversationHasVisibleMessages() {
    if (!Array.isArray(lxmfConversation)) return false;
    for (var i = 0; i < lxmfConversation.length; i++) {
        var m = lxmfConversation[i];
        if (!m) continue;
        if ((m.content || '').trim()) return true;
        if (m.image) return true;
        if (Array.isArray(m.attachments) && m.attachments.length > 0) return true;
        if (m.id) return true;
    }
    return false;
}

function _promoteGhostConversationRow(hash) {
    var container = document.getElementById('lxmf-conversations-list');
    if (!container) return;
    var row = container.querySelector('.msg-conv-row[data-hash="' + hash + '"]');
    if (row) row.removeAttribute('data-ghost');
    if (_ghostConversationHash === hash) _ghostConversationHash = null;
}

function _onChatDetailExit() {
    var exitingHash = lxmfActiveContact;
    _clearImageBlobUrlCache();
    var input = document.getElementById('lxmf-input');
    if (exitingHash) _saveActiveDraft(input);

    if (!_ghostConversationHash || _ghostConversationHash !== exitingHash) {
        _activateConversation(null, 'left_conversation');
        return;
    }

    if (_conversationHasVisibleMessages()) {
        _promoteGhostConversationRow(exitingHash);
        loadConversations();
        _activateConversation(null, 'left_conversation');
        return;
    }

    _removeGhostRow();
    cacheDel(exitingHash);
    _activateConversation(null, 'left_conversation');
    lxmfConversation = [];
    if (input) {
        input.value = '';
        input.style.height = '';
    }
    renderConversation();
    _renderConversationsFromCache(lxmfConversations || []);
}

function _refreshActiveConversationThreads() {
    if (RS.threads) RS.threads.refresh();
}

function _saveActiveDraft(input) {
    var key = _activeConversationKey();
    if (!input || !key) return;
    if (input.value.trim()) { _lxmfDrafts[key] = input.value; }
    else { delete _lxmfDrafts[key]; }
}

function _restoreActiveDraft(input) {
    if (!input) return;
    input.value = _lxmfDrafts[_activeConversationKey()] || '';
    input.style.height = '';
}

// The conversation a list row opens: the one holding the contact's newest message.
function _latestConversationThread(hash) {
    hash = _canonicalConversationHash(hash);
    var row = (lxmfConversations || []).find(function(c) {
        return c && _canonicalConversationHash(c.hash) === hash;
    });
    return row && row.thread ? _canonicalConversationThread(row.thread) : '';
}

// Switch the open chat to another conversation with the same contact.
function switchConversationThread(thread) {
    if (!lxmfActiveContact) return;
    thread = _canonicalConversationThread(thread);
    if (thread === lxmfActiveThread) return;
    var input = document.getElementById('lxmf-input');
    _saveActiveDraft(input);
    if (_replyTarget) clearReplyTarget();
    var hash = _activateConversation(lxmfActiveContact, 'navigation', thread).hash;
    _restoreActiveDraft(input);
    return _loadConversation(hash);
}
window.switchConversationThread = switchConversationThread;

function _openingUnreadCount(hash, thread) {
    var items = lxmfActiveThreads && Array.isArray(lxmfActiveThreads.items) ? lxmfActiveThreads.items : null;
    var item = items ? items.find(function(t) { return _canonicalConversationThread(t.thread) === thread; }) : null;
    if (item && hash === _canonicalConversationHash(lxmfActiveContact)) return Math.max(0, Number(item.unread) || 0);
    var row = (lxmfConversations || []).find(function(c) { return c && _canonicalConversationHash(c.hash) === hash; });
    if (!row || _canonicalConversationThread(row.thread || '') !== thread) return 0;
    return Math.max(0, Number(row.unread) || 0);
}

// Cache-first render of the open conversation with `hash` (activate it first)
// to avoid an empty-spinner flash; reconciles via fetch.
function _loadConversation(hash) {
    hash = _canonicalConversationHash(hash);
    var thread = _resolveConversationThread(hash);
    var key = _conversationKey(hash, thread);
    var loadOwner = _conversationOwnerSnapshot();
    var loadClears = _messagesClearedCount;
    // Reactions are keyed per message; drop the previous conversation's
    // entries so the map doesn't accumulate across switches.
    _msgReactions = {};
    // Read before get_conversation marks it read: where the new messages start.
    var unread = _openingUnreadCount(hash, thread);
    _unreadMarker = unread ? { key: key, count: unread, firstId: null, placed: false } : null;
    _jumpNewIds = [];
    _clearMessageSelection();
    var cached = cacheGet(key);
    if (cached && cached.length > 0) {
        lxmfConversation = cached;
    } else {
        lxmfConversation = [];
    }
    renderConversation({ forceScrollBottom: true });
    // get_conversation fetches messages AND marks-read; broadcasts unread_total.
    return RS.invoke('get_conversation', { hash: hash, thread: thread || null }).then(function(result) {
        if (!_conversationOwnerIsCurrent(loadOwner) || loadClears !== _messagesClearedCount) return;
        var messages = _mergeConversationMessages(key, (result && result.messages) || []);
        cacheSet(key, messages);
        if (key === _activeConversationKey()) {
            lxmfActiveThreads = (result && result.threads) || null;
            lxmfConversation = messages;
            renderConversation({ forceScrollBottom: true });
        }
    }).catch(function() {});
}


// The list the rows live in; the empty, loading, and error states replace it.
function _conversationListEl(container) {
    var list = container.querySelector('ul.msg-conv-list');
    if (list) return list;
    container.innerHTML = '<ul class="ui-list msg-conv-list" aria-label="Conversations"></ul>';
    return container.firstChild;
}

function _markActiveConversationRow(container, hash) {
    container.querySelectorAll('.msg-conv-row[aria-current]').forEach(function(r) { r.removeAttribute('aria-current'); });
    var row = hash ? container.querySelector('.msg-conv-row[data-hash="' + hash + '"]') : null;
    if (row) row.setAttribute('aria-current', 'true');
}

// Placeholder row shown until the backend confirms the conversation exists.
function _ensureGhostRow(hash) {
    var container = document.getElementById('lxmf-conversations-list');
    if (!container) return;

    if (_ghostConversationHash && _ghostConversationHash !== hash) {
        var oldGhost = container.querySelector('.msg-conv-row[data-ghost="true"]');
        if (oldGhost && oldGhost.parentNode) oldGhost.parentNode.remove();
    }

    if (container.querySelector('.msg-conv-row[data-hash="' + hash + '"]')) {
        _markActiveConversationRow(container, hash);
        _ghostConversationHash = null;
        return;
    }

    var list = _conversationListEl(container);
    list.insertAdjacentHTML('afterbegin', _conversationRowHtml({ hash: hash, last_message: '' }, { ghost: true }));
    _markActiveConversationRow(container, hash);
    _ghostConversationHash = hash;
}

function _updateConversationPreview(hash, previewText, timestamp) {
    var container = document.getElementById('lxmf-conversations-list');
    if (!container) return;
    var list = _conversationListEl(container);
    var row = container.querySelector('.msg-conv-row[data-hash="' + hash + '"]');
    var payload = Object.assign({}, _conversationPayloadForHash(hash) || { hash: hash }, {
        last_message: (previewText || '').substring(0, 200),
        last_direction: 'outbound',
        timestamp: timestamp,
        unread: 0
    });
    var html = _conversationRowHtml(payload, { current: true });
    if (row) row.parentNode.remove();
    list.insertAdjacentHTML('afterbegin', html);
    _markActiveConversationRow(container, hash);
    if (_ghostConversationHash === hash) {
        _ghostConversationHash = null;
    }
}

function _conversationPreviewForMessage(message) {
    if (!message) return '';
    var title = (message.title || '').trim();
    if (title) return _messagePlainContent(message, !!message.audio).trim();
    if (message.audio && (Number(message.audio.mode) === 0x10 || Number(message.audio.mode) === 0x03)) return 'Voice message';
    if (message.audio) return 'Unsupported audio';
    var content = (_messageIsMarkdown(message) ? _stripMessageMarkdown(message.content) : (message.content || '')).trim();
    if (content) return content;
    if (message.image) return 'Photo';
    if (Array.isArray(message.attachments) && message.attachments.length > 0) {
        return message.attachments[0].filename || 'Attachment';
    }
    return '';
}

function _conversationListPreview(c) {
    var text = c.last_message || '';
    return Number(c.preview_renderer) === 2 ? _stripMessageMarkdown(text) : text;
}

function _lastVisibleConversationMessage() {
    if (!Array.isArray(lxmfConversation)) return null;
    for (var i = lxmfConversation.length - 1; i >= 0; i--) {
        var m = lxmfConversation[i];
        if (!m) continue;
        if ((m.content || '').trim() || m.image || (Array.isArray(m.attachments) && m.attachments.length > 0) || m.id) {
            return m;
        }
    }
    return null;
}

function _optimisticConversationPayload() {
    if (!lxmfActiveContact) return null;
    var last = _lastVisibleConversationMessage();
    if (!last) return null;
    return {
        hash: lxmfActiveContact,
        display_name: null,
        is_contact: false,
        last_message: _conversationPreviewForMessage(last),
        last_direction: last.direction || 'outbound',
        timestamp: last.timestamp || (Date.now() / 1000),
        unread: 0,
    };
}

function _mergeOptimisticConversation(convos) {
    var rows = Array.isArray(convos) ? convos.slice() : [];
    var optimistic = _optimisticConversationPayload();
    if (!optimistic) return rows;
    for (var i = 0; i < rows.length; i++) {
        if (rows[i] && rows[i].hash === optimistic.hash) return rows;
    }
    rows.unshift(optimistic);
    return rows;
}

// Presence on a list avatar marks only people heard in the last two hours.
function _conversationPresence(hash) {
    var peer = _peerInfo(hash);
    if (!peer || !peer.last_seen) return { active: false, heard: 0 };
    return { active: RS.kit.presenceFor(peer.last_seen) === 'active', heard: peer.last_seen };
}

// One conversation row (the kit list row), shared by Messages and Home. opts: { current, ghost }.
function _conversationRowHtml(c, opts) {
    opts = opts || {};
    var hash = c.hash;
    var nameInfo = _conversationNameInfo(hash, c.display_name, c.is_contact);
    var presence = _conversationPresence(hash);
    var title = nameInfo.isHash
        ? '<span class="msg-title-hash" title="' + escapeHtml(RS.kit.hash.text(hash, 'full')) + '">' + escapeHtml(nameInfo.name) + '</span>'
        : ratspeakDisplayNameHtml(nameInfo.name, hash);
    var preview = _conversationListPreview(c);
    var sub = preview || c.thread_label
        ? (c.thread_label ? '<span class="msg-thread">' + escapeHtml(c.thread_label) + '</span> · ' : '') +
            (preview && c.last_direction === 'outbound' ? 'You: ' : '') + escapeHtml(preview)
        : 'No messages yet';
    var unread = Number(c.unread) || 0;
    var count = unread > 0
        ? '<span class="ui-count" aria-label="' + unread + ' unread">' + (unread > 99 ? '99+' : unread) + '</span>'
        : '';
    var time = c.timestamp
        ? '<span class="ui-time" title="' + escapeHtml(RS.kit.time.full(c.timestamp)) + '">' + escapeHtml(RS.kit.time.row(c.timestamp)) + '</span>'
        : '';
    return '<li><button type="button" class="ui-row msg-conv-row' + (unread > 0 ? ' is-unread' : '') + '" data-hash="' + escapeHtml(hash) + '" data-presence="' + (presence.active ? 'active' : '') + '"' +
        (opts.current ? ' aria-current="true"' : '') + (opts.ghost ? ' data-ghost="true"' : '') + '>' +
        RS.kit.avatar(hash, 'row', presence.active ? 'active' : null, presence.heard) +
        '<span class="ui-row-main"><span class="ui-row-top"><span class="ui-row-title">' + title + '</span>' + time + '</span>' +
        '<span class="ui-row-sub"><span class="t">' + sub + '</span>' + count + '</span></span></button></li>';
}

function _openConversationFromRow(hash) {
    if (_convSwipedRecently || !hash) return;
    if (_ghostConversationHash && _ghostConversationHash !== hash) _removeGhostRow();
    // Per-conversation draft persistence on switch.
    var input = document.getElementById('lxmf-input');
    _saveActiveDraft(input);
    hash = _activateConversation(hash, 'navigation', _latestConversationThread(hash)).hash;
    _restoreActiveDraft(input);
    var container = document.getElementById('lxmf-conversations-list');
    if (container) _markActiveConversationRow(container, hash);
    _loadConversation(hash);
    loadConversations();
    if (isCompactLayout()) {
        RS.viewStack.push('chat-detail', { meta: { contactHash: hash } });
        history.pushState({ view: 'message', detail: true }, '', '#message');
    }
}

function _conversationListStateHtml(state) {
    if (state === 'loading') {
        return '<div class="ui-empty ui-empty--inline" role="status"><span class="ui-spin ui-spin--ring" aria-hidden="true"></span><span>Loading conversations</span></div>';
    }
    if (state === 'error') {
        return '<div class="ui-empty" data-list-state="error"><span class="ui-tile ui-tile--lg ui-tile--error">' + RS.kit.icon('alert') + '</span>' +
            '<h3 class="ui-empty-title">Couldn’t load your conversations</h3>' +
            '<p class="ui-empty-body">Ratspeak couldn’t read them from this device.</p>' +
            '<div class="ui-empty-actions"><button type="button" class="ui-btn" data-list-action="retry">' + RS.kit.icon('refresh') + 'Try again</button></div></div>';
    }
    return '<div class="ui-empty" data-list-state="empty"><span class="ui-tile ui-tile--lg">' + RS.kit.icon('message') + '</span>' +
        '<h3 class="ui-empty-title">No conversations yet</h3>' +
        '<p class="ui-empty-body">Message a contact, someone you found in Peers, or any LXMF address.</p>' +
        '<div class="ui-empty-actions"><button type="button" class="ui-btn ui-btn--primary" data-list-action="new">' + RS.kit.icon('compose') + 'New message</button>' +
        '<button type="button" class="ui-link ui-phone-only" data-list-action="share">Share your contact card</button></div></div>';
}

function renderCockpitConversations() {
    renderDashboardRecentMessages();
}

// Home's Recent messages: the five latest conversations, as the Messages list draws them.
function renderDashboardRecentMessages() {
    var container = document.getElementById('dashboard-recent-messages');
    if (!container) return;

    var convos = lxmfConversations;
    var viewAll = document.getElementById('dash-view-all-messages');
    if (viewAll) viewAll.hidden = !convos || convos.length === 0;
    if (!convos || convos.length === 0) {
        container.innerHTML = '<div class="ui-empty">' +
            '<span class="ui-tile ui-tile--lg">' + RS.kit.icon('message') + '</span>' +
            '<h3 class="ui-empty-title">No messages yet</h3>' +
            '<p class="ui-empty-body">Write to someone nearby, or to anyone by their LXMF address. Every message is end-to-end encrypted.</p>' +
            '<div class="ui-empty-actions"><button type="button" class="ui-btn" data-home-new-message>' + RS.kit.icon('compose') + 'New message</button></div>' +
        '</div>';
        return;
    }

    container.innerHTML = '<ul class="ui-list" aria-label="Recent messages">' +
        convos.slice(0, 5).map(function(c) { return _conversationRowHtml(c); }).join('') + '</ul>';
    container.querySelectorAll('.msg-conv-row').forEach(function(el) {
        el.addEventListener('click', function() {
            var hash = this.dataset.hash;
            hash = _activateConversation(hash, 'navigation', _latestConversationThread(hash)).hash;
            switchView('message');
            _loadConversation(hash);
            loadConversations();
        });
    });
}

var _loadConversationsTimer = null;
var _conversationsRetryTimer = null;
var _lastConversationsLoad = 0;
var _conversationsFirstLoadDone = false;
var _conversationsRevision = 0;
// Home and Messages share one snapshot and at most one outstanding IPC read.
var _convFetchInFlight = false;
var _convFetchAgain = false;

function _cancelConversationsTimers() {
    clearTimeout(_loadConversationsTimer);
    clearTimeout(_conversationsRetryTimer);
    _loadConversationsTimer = null;
    _conversationsRetryTimer = null;
}

function _resetConversationList() {
    _cancelConversationsTimers();
    _conversationsRevision += 1;
    _conversationsFirstLoadDone = false;
    _lastConversationsLoad = 0;
    lxmfConversations = [];
    // Retain the in-flight lease until the old IPC actually settles. Queue the
    // new identity's read instead of pretending the old request was cancelled.
    _convFetchAgain = _convFetchInFlight;
    _renderConversationsFromCache(lxmfConversations);
    renderDashboardRecentMessages();
}

function _acceptConversations(convos) {
    _conversationsRevision += 1;
    _conversationsFirstLoadDone = true;
    lxmfConversations = Array.isArray(convos) ? convos : [];
    _cancelConversationsTimers();
    _renderConversationsFromCache(lxmfConversations);
    renderDashboardRecentMessages();
}

function loadConversationsForce() {
    _cancelConversationsTimers();
    _lastConversationsLoad = 0;
    _loadConversationsReal();
}

function loadConversations() {
    var convList = document.getElementById('lxmf-conversations-list');
    if (convList && !convList._ptrAttached) {
        RS.gestures.attachPullToRefresh(convList, { onRefresh: loadConversations });
    }
    var now = Date.now();
    if (now - _lastConversationsLoad < 500) {
        if (!_loadConversationsTimer) {
            _loadConversationsTimer = setTimeout(function() {
                _loadConversationsTimer = null;
                _loadConversationsReal();
            }, 300);
        }
        return;
    }
    clearTimeout(_loadConversationsTimer);
    _loadConversationsTimer = null;
    _loadConversationsReal();
}

function _loadConversationsReal(retryCount) {
    if (_convFetchInFlight) { _convFetchAgain = true; return; }
    clearTimeout(_conversationsRetryTimer);
    _conversationsRetryTimer = null;
    var container = document.getElementById('lxmf-conversations-list');
    if (!container) return;
    retryCount = retryCount || 0;
    _convFetchInFlight = true;
    _convFetchAgain = false;
    _lastConversationsLoad = Date.now();
    var owner = _conversationOwnerSnapshot();
    var revision = _conversationsRevision;
    function current() {
        return _conversationOwnerIdentityIsCurrent(owner) && revision === _conversationsRevision;
    }
    function scheduleRetry(delay) {
        _conversationsRetryTimer = setTimeout(function() {
            _conversationsRetryTimer = null;
            if (current()) _loadConversationsReal(retryCount + 1);
        }, delay);
    }
    RS.invoke('api_lxmf_conversations').then(function(convos) {
        // A mutation broadcast or identity replacement supersedes older reads.
        if (!current()) return;
        if (!_conversationsFirstLoadDone && (!convos || convos.length === 0) && retryCount < 2) {
            scheduleRetry(1500);
            return;
        }
        _acceptConversations(convos);
    }).catch(function(err) {
        if (!current()) return;
        if (retryCount < 2) {
            scheduleRetry(Math.min(2000, (retryCount + 1) * 1000));
            return;
        }
        if (lxmfConversations.length > 0) {
            _renderConversationsFromCache(lxmfConversations);
        } else if (!container.querySelector('.msg-conv-row')) {
            container.innerHTML = _conversationListStateHtml('error');
        }
    }).finally(function() {
        _convFetchInFlight = false;
        if (_convFetchAgain) _loadConversationsReal();
    });
}

function _renderConversationsFromCache(convos) {
    var container = document.getElementById('lxmf-conversations-list');
    if (!container) return;
    _bindConversationList(container);

    convos = _mergeOptimisticConversation(convos);
    convos = convos.map(function(conversation) {
        if (!conversation || typeof conversation !== 'object') return conversation;
        conversation.hash = _canonicalConversationHash(conversation.hash);
        return conversation;
    });

    if (!convos || convos.length === 0) {
        if (_ghostConversationHash && _ghostConversationHash === lxmfActiveContact) {
            container.innerHTML = '';
            _ensureGhostRow(_ghostConversationHash);
        } else {
            container.innerHTML = _conversationListStateHtml(_conversationsFirstLoadDone ? 'empty' : 'loading');
        }
        _renderFirstRunDetail();
        return;
    }

    container.innerHTML = '<ul class="ui-list msg-conv-list" aria-label="Conversations">' + convos.map(function(c) {
        return _conversationRowHtml(c, { current: lxmfActiveContact === c.hash });
    }).join('') + '</ul>';

    // Re-inject ghost row if the server list doesn't yet include the conversation.
    if (_ghostConversationHash && _ghostConversationHash === lxmfActiveContact) {
        if (!container.querySelector('.msg-conv-row[data-hash="' + _ghostConversationHash + '"]')) {
            _ensureGhostRow(_ghostConversationHash);
        }
    }
    _renderFirstRunDetail();
}

// One delegated handler: rows open their conversation, the empty and error states act.
function _bindConversationList(container) {
    if (container._convListBound) return;
    container._convListBound = true;
    container.addEventListener('click', function(e) {
        var row = e.target.closest && e.target.closest('.msg-conv-row');
        if (row) {
            _openConversationFromRow(row.dataset.hash);
            return;
        }
        var action = e.target.closest && e.target.closest('[data-list-action]');
        if (!action) return;
        var kind = action.getAttribute('data-list-action');
        if (kind === 'retry') {
            container.innerHTML = _conversationListStateHtml('loading');
            loadConversationsForce();
        } else if (kind === 'new') {
            openNewMessageSheet();
        } else if (kind === 'share' && typeof openActiveIdentityContactCard === 'function') {
            openActiveIdentityContactCard();
        }
    });
    RS.gestures.attachSwipe(container, {
        delegated: '.msg-conv-row',
        direction: 'left',
        distanceThreshold: RS.gestures.SWIPE_DISTANCE_CONV_DELETE_PX,
        hapticAt: { commit: 'warning' },
        onProgress: function(dx, _dy, _progress, row) {
            if (!row) return;
            var offset = Math.max(0, Math.min(-dx, 120));
            row.style.transform = 'translateX(-' + offset + 'px)';
            row.style.transition = 'none';
            _addConvDeleteIndicator(row);
            var del = row.querySelector('.conv-swipe-delete');
            if (del) del.style.opacity = Math.min(1, offset / 80);
        },
        onCommit: function(row) {
            if (!row) return;
            _suppressNextConvClick();
            var hash = row.dataset.hash;
            if (!hash) {
                _resetConvRowVisual(row);
                return;
            }
            var name = _conversationNameInfo(hash, null, false).name;
            _resetConvRowVisual(row);
            showConversationDeleteDialog(hash, name);
        },
        onCancel: function(dx, _dy, row) {
            if (!row) return;
            // Half-swipe still suppresses synthetic click.
            if (dx < -10) _suppressNextConvClick();
            _resetConvRowVisual(row);
        }
    });
}

// T2-3: per-poll full re-renders are gated. Hidden views skip the rebuild
// entirely (the switchView hooks repaint on activation); visible repaints
// are deduped against the freshly built markup. Counters back the source
// contract test and manual QA.
var _renderGate = { skippedHidden: 0, skippedClean: 0, painted: 0 };

function _gateHidden(viewId, container) {
    var view = document.getElementById(viewId);
    if (view && !view.classList.contains('active')) {
        _renderGate.skippedHidden++;
        if (container) container._rsLastHtml = null;
        return true;
    }
    return false;
}

function _gateClean(container, html) {
    if (container._rsLastHtml === html) {
        _renderGate.skippedClean++;
        return true;
    }
    container._rsLastHtml = html;
    _renderGate.painted++;
    return false;
}

// Home: the header actions, each card's View all, the empty-state actions, and the rows.
document.addEventListener('DOMContentLoaded', function() {
    var home = document.getElementById('view-dashboard');
    if (!home) return;
    home.addEventListener('click', function(e) {
        var target = e.target.closest('button');
        if (!target || !home.contains(target)) return;
        if (target.id === 'dash-announce' || target.hasAttribute('data-home-announce')) {
            tryTriggerAnnounce();
        } else if (target.id === 'dash-new-message' || target.hasAttribute('data-home-new-message') ||
                target.id === 'dash-view-all-messages') {
            switchView('message');
        } else if (target.id === 'dash-add-connection' || target.hasAttribute('data-home-add-connection') ||
                target.hasAttribute('data-home-open-network')) {
            switchView('network');
        } else if (target.id === 'dash-view-network') {
            switchView('peers');
        } else if (target.closest('#dashboard-peers-list') && target.dataset.hash) {
            if (typeof openConversationWith === 'function') openConversationWith(target.dataset.hash);
        }
    });
});

window.renderConversation = renderConversation;

function _messageBodyContent(msg, hasRenderedAudio) {
    var displayContent = (msg && msg.content) || '';
    if (msg && (msg.image || (msg.attachments && msg.attachments.length > 0)) && displayContent) {
        displayContent = displayContent.replace(/\n?\[File:[^\]]*\]\s*$/, '');
    }
    if (hasRenderedAudio && displayContent.trim() === 'Voice message') displayContent = '';
    return displayContent;
}

function _messageDisplayContent(msg, hasRenderedAudio) {
    var displayContent = _messageBodyContent(msg, hasRenderedAudio);
    var title = ((msg && msg.title) || '').trim();
    if (title) return title + (displayContent.trim() ? '\n\n' + displayContent : '');
    return displayContent;
}

// Copy keeps Markdown source, but spoilers still covered in this bubble stay hidden.
function _messageCopyText(msg, content) {
    var text = _messageDisplayContent(msg, !!(msg && msg.audio));
    var spoilers = content && content.querySelectorAll ? content.querySelectorAll('.md-spoiler') : [];
    if (!_messageIsMarkdown(msg) || !spoilers.length) return text;
    var body = RS.markdown.redactSpoilers(_messageBodyContent(msg, !!msg.audio), function(i) {
        return !!(spoilers[i] && spoilers[i].classList.contains('is-revealed'));
    });
    var title = ((msg && msg.title) || '').trim();
    return title ? title + (body.trim() ? '\n\n' + body : '') : body;
}

// Previews, reply quotes and search show Markdown messages without syntax.
function _messagePlainContent(msg, hasRenderedAudio) {
    if (!_messageIsMarkdown(msg)) return _messageDisplayContent(msg, hasRenderedAudio);
    var body = _stripMessageMarkdown(_messageBodyContent(msg, hasRenderedAudio));
    var title = ((msg && msg.title) || '').trim();
    if (title) return title + (body.trim() ? '\n\n' + body : '');
    return body;
}

function _mobileMessageActionsUseLongPress() {
    return (typeof isTauriMobile === 'function' && isTauriMobile()) ||
        (typeof isMobile === 'function' && isMobile());
}

function _mergeConversationRenderOptions(previous, next) {
    previous = previous || {};
    next = next || {};
    return {
        forceScrollBottom: !!(previous.forceScrollBottom || next.forceScrollBottom),
        stickToBottom: !!(previous.stickToBottom || next.stickToBottom),
    };
}

function _activeActionOwnsCurrentMessage() {
    if (!_activeContextMenu || !_activeContextMenu.msgId) return false;
    if (_activeContextMenu.ownerHash !== _canonicalConversationHash(lxmfActiveContact)) return false;
    return lxmfConversation.some(function(message) {
        return message.id === _activeContextMenu.msgId;
    });
}

function _deferConversationRender(options) {
    var ownerHash = _canonicalConversationHash(lxmfActiveContact);
    if (!_deferredConversationRenderOptions || _deferredConversationRenderOwnerHash !== ownerHash) {
        if (_deferredConversationRenderOptions &&
                _deferredConversationRenderOwnerHash !== ownerHash) {
            _cancelScheduledDeferredConversationRender(_deferredConversationRenderGeneration);
        }
        _deferredConversationRenderOptions = null;
        _deferredConversationRenderOwnerHash = ownerHash;
        _deferredConversationRenderGeneration++;
    }
    _deferredConversationRenderOptions = _mergeConversationRenderOptions(
        _deferredConversationRenderOptions,
        options
    );
}

function _pendingRenderReleaseOwnsCurrentConversation() {
    return !!(_deferredConversationRenderRelease &&
        _deferredConversationRenderOptions &&
        _deferredConversationRenderRelease.ownerHash === _canonicalConversationHash(lxmfActiveContact) &&
        _deferredConversationRenderRelease.generation === _deferredConversationRenderGeneration);
}

function _deferActiveMessageInteractionRender(options) {
    if (!_activeActionOwnsCurrentMessage() &&
            !_pendingRenderReleaseOwnsCurrentConversation()) return false;
    _deferConversationRender(options);
    return true;
}

function _clearDeferredConversationRender(expectedGeneration) {
    if (typeof expectedGeneration === 'number' &&
            expectedGeneration !== _deferredConversationRenderGeneration) return false;
    _deferredConversationRenderOptions = null;
    _deferredConversationRenderOwnerHash = null;
    return true;
}

function _takeDeferredConversationRenderOptions(options) {
    var ownerHash = _canonicalConversationHash(lxmfActiveContact);
    var deferredOptions = _deferredConversationRenderOwnerHash === ownerHash
        ? _deferredConversationRenderOptions
        : null;
    if (_deferredConversationRenderOptions) {
        _cancelScheduledDeferredConversationRender(_deferredConversationRenderGeneration);
    }
    var merged = _mergeConversationRenderOptions(deferredOptions, options);
    _clearDeferredConversationRender();
    return merged;
}

function _cancelScheduledDeferredConversationRender(expectedGeneration) {
    if (!_deferredConversationRenderRelease) return false;
    if (typeof expectedGeneration === 'number' &&
            expectedGeneration !== _deferredConversationRenderRelease.generation) return false;
    _deferredConversationRenderRelease.cleanup();
    _deferredConversationRenderRelease = null;
    return true;
}

function _scheduleDeferredConversationRenderAfterPointer() {
    if (_deferredConversationRenderRelease) return false;
    // Establish the release lease even before the first network/progress render.
    // A neutral intent is cheap and ensures that first update cannot detach the
    // pointer's eventual click target.
    if (!_deferredConversationRenderOptions) _deferConversationRender({});
    var ownerHash = _canonicalConversationHash(lxmfActiveContact);
    var generation = _deferredConversationRenderGeneration;
    var record = null;
    function removeReleaseListeners() {
        document.removeEventListener('pointerup', release, true);
        document.removeEventListener('pointercancel', cancelRelease, true);
        document.removeEventListener('mouseup', release, true);
        document.removeEventListener('touchend', release, true);
        document.removeEventListener('touchcancel', cancelRelease, true);
    }
    function removeClickListener() {
        document.removeEventListener('click', clickAfterRelease, true);
    }
    function runFlush() {
        if (!record) return;
        record.cleanup();
        if (_deferredConversationRenderRelease === record) {
            _deferredConversationRenderRelease = null;
        }
        if (ownerHash !== _canonicalConversationHash(lxmfActiveContact)) {
            _clearDeferredConversationRender(generation);
            return;
        }
        _flushDeferredConversationRender(generation);
    }
    function scheduleFlush() {
        if (!record || record.flushScheduled) return;
        record.flushScheduled = true;
        removeReleaseListeners();
        removeClickListener();
        if (record.fallbackTimer !== null) clearTimeout(record.fallbackTimer);
        record.fallbackTimer = null;
        record.timer = setTimeout(runFlush, 0);
    }
    function release() {
        if (!record || record.released) return;
        record.released = true;
        removeReleaseListeners();
        document.addEventListener('click', clickAfterRelease, true);
        record.fallbackTimer = setTimeout(runFlush, 450);
    }
    function cancelRelease() {
        if (!record || record.released) return;
        record.released = true;
        scheduleFlush();
    }
    function clickAfterRelease() {
        scheduleFlush();
    }
    record = {
        ownerHash: ownerHash,
        generation: generation,
        released: false,
        flushScheduled: false,
        timer: null,
        fallbackTimer: null,
        cleanup: function() {
            removeReleaseListeners();
            removeClickListener();
            if (record.timer !== null) clearTimeout(record.timer);
            if (record.fallbackTimer !== null) clearTimeout(record.fallbackTimer);
        },
    };
    _deferredConversationRenderRelease = record;
    document.addEventListener('pointerup', release, true);
    document.addEventListener('pointercancel', cancelRelease, true);
    document.addEventListener('mouseup', release, true);
    document.addEventListener('touchend', release, true);
    document.addEventListener('touchcancel', cancelRelease, true);
    return true;
}

function _flushDeferredConversationRender(expectedGeneration) {
    if (!_deferredConversationRenderOptions) return false;
    if (typeof expectedGeneration === 'number' &&
            expectedGeneration !== _deferredConversationRenderGeneration) return false;
    if (_deferredConversationRenderOwnerHash !== _canonicalConversationHash(lxmfActiveContact)) {
        _clearDeferredConversationRender(expectedGeneration);
        return false;
    }
    _cancelScheduledDeferredConversationRender(expectedGeneration);
    var options = _deferredConversationRenderOptions;
    _clearDeferredConversationRender(expectedGeneration);
    renderConversation(options);
    return true;
}

// Hover actions (pointer only): "+" expands in place into React, Reply, and More. It collapses on
// Escape, a click away, focus leaving, or the pointer leaving the bubble after a short grace;
// while its reactions or menu are open, leaving keeps it.
var MSG_HOVER_LEAVE_GRACE_MS = 280;
var _hoverLeaveTimer = null;

function _hoverButtons(bar) {
    return Array.prototype.slice.call(bar.querySelectorAll('.msg-hover-set > button'));
}

function _hoverOwnsMenu(bar) {
    return !!(_activeContextMenu && _activeContextMenu.hoverBar === bar);
}

function _cancelHoverLeave() {
    if (_hoverLeaveTimer) clearTimeout(_hoverLeaveTimer);
    _hoverLeaveTimer = null;
}

function _closeHoverBar(bar) {
    if (!bar) return;
    _cancelHoverLeave();
    bar.classList.remove('is-open');
    bar._pointerIn = false;
    var plus = bar.querySelector('.msg-hover-plus');
    if (plus) plus.setAttribute('aria-expanded', 'false');
    _hoverButtons(bar).forEach(function(btn) {
        if (btn.hasAttribute('aria-expanded')) btn.setAttribute('aria-expanded', 'false');
    });
}

function _openHoverBar(bar, byPointer, focusFirst) {
    document.querySelectorAll('#lxmf-messages .msg-hover.is-open').forEach(function(other) {
        if (other !== bar) _closeHoverBar(other);
    });
    _cancelHoverLeave();
    bar.classList.add('is-open');
    bar._pointerIn = !!byPointer;
    var plus = bar.querySelector('.msg-hover-plus');
    if (plus) plus.setAttribute('aria-expanded', 'true');
    var buttons = _hoverButtons(bar);
    buttons.forEach(function(btn, i) { btn.tabIndex = i ? -1 : 0; });
    if (focusFirst !== false && buttons[0]) _focusMessageControl(buttons[0]);
}

function _openHoverMessageId(container) {
    var bar = container && container.querySelector('.msg-hover.is-open');
    if (!bar) return null;
    var focused = bar.contains(document.activeElement) && document.activeElement.getAttribute('data-hover-act');
    var bubble = bar.closest('.msg-bubble');
    return bubble ? { id: bubble.getAttribute('data-msg-id'), act: focused || '', pointer: !!bar._pointerIn } : null;
}

function _reopenHoverBar(container, open) {
    var bubble = _findRenderedMessageBubble(container, open.id);
    var bar = bubble && bubble.querySelector('.msg-hover');
    if (!bar) return;
    _openHoverBar(bar, open.pointer, false);
    if (open.act) _focusMessageControl(bar.querySelector('[data-hover-act="' + open.act + '"]'));
}

function _messageForElement(el) {
    var bubble = el && el.closest ? el.closest('.msg-bubble[data-msg-id]') : null;
    var msg = bubble ? _conversationMessageById(bubble.getAttribute('data-msg-id')) : null;
    return { bubble: bubble, msg: msg };
}

function _runHoverAction(btn, event) {
    var bar = btn.closest('.msg-hover');
    var found = _messageForElement(btn);
    if (!found.msg) return;
    var act = btn.getAttribute('data-hover-act');
    var wasOpen = btn.getAttribute('aria-expanded') === 'true';
    if (_hoverOwnsMenu(bar)) _dismissContextMenu({ restoreFocus: false });
    if (wasOpen) return;
    if (act === 'reply') {
        _closeHoverBar(bar);
        setReplyTarget(found.msg);
        return;
    }
    var rect = btn.getBoundingClientRect();
    _showMsgContextMenu(found.msg, rect.left, rect.bottom, found.bubble, btn, {
        focusDialog: _messageActivationExpectsFocus(event),
        anchor: act === 'more' ? btn : null,
        reactOnly: act === 'react',
    });
}

// The jump button: shown away from the latest, counting new messages still below the view.
function _syncJumpButton(container) {
    var btn = document.getElementById('lxmf-jump-btn');
    if (!btn || !container) return;
    var away = !!lxmfActiveContact && lxmfConversation.length > 0 && container.clientHeight > 0 &&
        !RS.chatScroll.nearBottom(container);
    if (!away) {
        btn.hidden = true;
        _jumpNewIds = [];
        return;
    }
    var ids = _jumpNewIds.slice();
    var marker = _unreadMarker && _unreadMarker.key === _activeConversationKey() ? _unreadMarker.firstId : null;
    if (marker) {
        var seen = false;
        lxmfConversation.forEach(function(m) {
            if (m.id === marker) seen = true;
            if (seen && m.direction === 'inbound' && ids.indexOf(m.id) === -1) ids.push(m.id);
        });
    }
    var bottom = container.getBoundingClientRect().bottom;
    var below = ids.filter(function(id) {
        var bubble = _findRenderedMessageBubble(container, id);
        return bubble && bubble.getBoundingClientRect().top > bottom - 24;
    }).length;
    var badge = btn.querySelector('.ui-count');
    if (badge) {
        badge.hidden = !below;
        badge.textContent = below > 99 ? '99+' : String(below);
    }
    btn.setAttribute('aria-label', 'Jump to latest' + (below ? ', ' + below + (below === 1 ? ' new message' : ' new messages') : ''));
    btn.hidden = false;
}

// One set of delegated listeners for everything inside the transcript.
function _wireTranscript(container) {
    if (container._rsTranscriptWired) return;
    container._rsTranscriptWired = true;

    container.addEventListener('click', function(e) {
        var t = e.target;
        if (!t || !t.closest) return;
        var plus = t.closest('.msg-hover-plus');
        if (plus) {
            _openHoverBar(plus.closest('.msg-hover'), e.detail > 0, true);
            return;
        }
        var act = t.closest('.msg-hover-set > button');
        if (act) {
            _runHoverAction(act, e);
            return;
        }
        var quote = t.closest('.msg-quote');
        if (quote) {
            if (_consumePendingMessageHoldActivation(e, quote)) return;
            var targetId = quote.getAttribute('data-reply-id');
            var targetEl = targetId ? _findRenderedMessageBubble(container, targetId) : null;
            if (targetEl) {
                var scrollBlock = isCompactLayout() ? 'nearest' : 'center';
                setTimeout(function() {
                    targetEl.scrollIntoView({ behavior: 'smooth', block: scrollBlock });
                }, 100);
                targetEl.classList.add('msg-highlight');
                setTimeout(function() { targetEl.classList.remove('msg-highlight'); }, 1500);
            }
            return;
        }
        var save = t.closest('.msg-file-save[data-stored-name]');
        if (save) {
            e.preventDefault();
            if (_consumePendingMessageHoldActivation(e, save)) return;
            var name = save.getAttribute('data-stored-name');
            RS.saveFile(name).then(function(saved) {
                if (saved !== false && typeof showToast === 'function') showToast('Saved', 'toast-success', 2200);
            }).catch(function(err) {
                if (typeof showToast === 'function') {
                    showToast('Could not download: ' + (err.message || err.code || 'unknown error'), 'toast-error', 4000);
                } else {
                    window.RS.diag('error', '[lxmf] file download failed:', name, err);
                }
            });
            return;
        }
        var image = t.closest('.msg-image-btn');
        if (image) {
            var img = image.querySelector('img.msg-img');
            if (!img || _consumePendingMessageHoldActivation(e, img)) return;
            e.stopPropagation();
            if (typeof openImageViewer === 'function') openImageViewer(img);
            return;
        }
        var chatAction = t.closest('[data-chat-action]');
        if (chatAction) {
            var kind = chatAction.getAttribute('data-chat-action');
            if (kind === 'show-qr' && typeof openActiveIdentityContactCard === 'function') openActiveIdentityContactCard();
            if (kind === 'add-contact' && lxmfActiveContact) _promptContactName(lxmfActiveContact, null);
        }
    });

    container.addEventListener('keydown', function(e) {
        var t = e.target;
        if (!t || !t.closest) return;
        var bar = t.closest('.msg-hover');
        if (e.key === 'Escape') {
            var open = bar && bar.classList.contains('is-open') ? bar : container.querySelector('.msg-hover.is-open');
            if (open && !_hoverOwnsMenu(open)) {
                e.preventDefault();
                e.stopPropagation();
                _closeHoverBar(open);
                _focusMessageControl(open.querySelector('.msg-hover-plus'));
            }
            return;
        }
        if (bar) {
            var buttons = _hoverButtons(bar);
            var at = buttons.indexOf(t);
            var to = { ArrowRight: at + 1, ArrowDown: at + 1, ArrowLeft: at - 1, ArrowUp: at - 1, Home: 0, End: buttons.length - 1 }[e.key];
            if (at === -1 || to == null) return;
            e.preventDefault();
            to = (to + buttons.length) % buttons.length;
            buttons.forEach(function(btn, k) { btn.tabIndex = k === to ? 0 : -1; });
            buttons[to].focus();
            return;
        }
        if (!(e.key === 'ContextMenu' || (e.shiftKey && e.key === 'F10'))) return;
        var found = _messageForElement(t);
        if (!found.msg) return;
        e.preventDefault();
        var rect = found.bubble.getBoundingClientRect();
        _showMsgContextMenu(found.msg, rect.left, rect.bottom, found.bubble, found.bubble, {
            focusDialog: true,
            anchor: found.bubble,
        });
    });

    container.addEventListener('focusout', function(e) {
        var bar = e.target && e.target.closest && e.target.closest('.msg-hover.is-open');
        if (!bar || _hoverOwnsMenu(bar)) return;
        if (e.relatedTarget && !bar.contains(e.relatedTarget)) _closeHoverBar(bar);
    });

    // The pointer zone is the bubble, which holds the bar.
    document.addEventListener('pointerover', function(e) {
        var bar = container.querySelector('.msg-hover.is-open');
        if (!bar) return;
        var bubble = bar.closest('.msg-bubble');
        if (bubble && bubble.contains(e.target)) {
            bar._pointerIn = true;
            _cancelHoverLeave();
            return;
        }
        if (!bar._pointerIn || _hoverOwnsMenu(bar)) return;
        _cancelHoverLeave();
        _hoverLeaveTimer = setTimeout(function() {
            _hoverLeaveTimer = null;
            if (bar.classList.contains('is-open') && !_hoverOwnsMenu(bar)) _closeHoverBar(bar);
        }, MSG_HOVER_LEAVE_GRACE_MS);
    });
    document.addEventListener('click', function(e) {
        var bar = container.querySelector('.msg-hover.is-open');
        if (!bar || bar.contains(e.target) || _hoverOwnsMenu(bar)) return;
        if (_activeContextMenu && _activeContextMenu.menu.contains(e.target)) return;
        _closeHoverBar(bar);
    });

    var jumpFrame = 0;
    container.addEventListener('scroll', function() {
        if (jumpFrame) return;
        jumpFrame = requestAnimationFrame(function() {
            jumpFrame = 0;
            _syncJumpButton(container);
        });
    }, { passive: true });
    var jump = document.getElementById('lxmf-jump-btn');
    if (jump) jump.addEventListener('click', function() {
        _jumpNewIds = [];
        RS.chatScroll.pinToBottom(container);
        _syncJumpButton(container);
    });
}

// The person, in the chat header: name or short address, and the details label.
function _renderChatHeaderName() {
    if (!lxmfActiveContact) return;
    var info = _conversationNameInfo(lxmfActiveContact, null, false);
    var nameEl = document.getElementById('lxmf-chat-header-name');
    if (nameEl) {
        var html = info.isHash
            ? '<span class="msg-title-hash" title="' + escapeHtml(RS.kit.hash.text(lxmfActiveContact, 'full')) + '">' + escapeHtml(info.name) + '</span>'
            : ratspeakDisplayNameHtml(info.name, lxmfActiveContact);
        if (nameEl.innerHTML !== html) nameEl.innerHTML = html;
    }
    var who = document.getElementById('lxmf-chat-who');
    if (who) who.setAttribute('aria-label', info.name + ', details');
    var input = document.getElementById('lxmf-input');
    if (input) {
        var first = info.isHash ? '' : String(info.name || '').trim().split(/\s+/)[0];
        var placeholder = first ? 'Message ' + first : 'Message';
        if (input.placeholder !== placeholder) input.placeholder = placeholder;
    }
    var intro = document.querySelector('#lxmf-messages .msg-intro-name');
    if (intro) intro.innerHTML = nameEl ? nameEl.innerHTML : escapeHtml(info.name);
}

// Nothing open: a quiet prompt, or on first run what people need to message you.
function _noConversationHtml() {
    var none = _conversationsFirstLoadDone && (!lxmfConversations || lxmfConversations.length === 0);
    var own = lxmfIdentity ? (lxmfIdentity.hash || lxmfIdentity.lxmf_hash || '') : '';
    if (none && own) {
        return '<div class="msg-first"><div class="ui-card"><div class="ui-card-head"><h3 class="ui-card-title">Your LXMF address</h3></div>' +
            '<div class="msg-first-addr">' + RS.kit.hash.copy(own, { form: 'full', noun: 'LXMF address' }) + '</div>' +
            '<p class="ui-help">Give it to people so they can message you, or show them your QR code.</p>' +
            '<button type="button" class="ui-btn" data-chat-action="show-qr">' + RS.kit.icon('qr') + 'Show QR code</button></div></div>';
    }
    return '<div class="ui-empty ui-empty--quiet ui-empty--center"><h3 class="ui-empty-title">Select a conversation</h3>' +
        '<p class="ui-empty-body">Or start a new one with the compose button.</p></div>';
}

function _renderFirstRunDetail() {
    if (lxmfActiveContact) return;
    var container = document.getElementById('lxmf-messages');
    if (!container) return;
    var html = _noConversationHtml();
    if (container._rsNoConversationHtml !== html) {
        container._rsNoConversationHtml = html;
        container.innerHTML = html;
    }
}

// A conversation with no messages yet: the person, one line on encryption, and Add contact.
function _conversationIntroHtml(hash) {
    var info = _conversationNameInfo(hash, null, false);
    var notContact = !_lookupContactRecord(hash);
    var name = info.isHash
        ? '<span class="msg-title-hash">' + escapeHtml(info.name) + '</span>'
        : ratspeakDisplayNameHtml(info.name, hash);
    return '<div class="msg-intro">' + RS.kit.avatar(hash, 64) +
        '<h3 class="msg-intro-name">' + name + '</h3>' +
        '<span class="msg-intro-sub">' + RS.kit.hash.copy(hash, { form: 'fit', noun: 'LXMF address' }) + '</span>' +
        '<p class="msg-intro-body">' + (notContact ? 'They aren’t in your contacts yet. ' : '') +
            'This is the start of your conversation. Messages are end-to-end encrypted.</p>' +
        (notContact ? '<button type="button" class="ui-btn" data-chat-action="add-contact">' + RS.kit.icon('user-plus') + 'Add contact</button>' : '') +
    '</div>';
}

// The open conversation's first unread message, counted back from the newest inbound ones.
function _unreadMarkerFor(msgs) {
    if (!_unreadMarker || _unreadMarker.key !== _activeConversationKey() || !_unreadMarker.count) return null;
    if (_unreadMarker.firstId) return _unreadMarker.firstId;
    var left = _unreadMarker.count;
    for (var i = msgs.length - 1; i >= 0 && left > 0; i--) {
        if (msgs[i].direction === 'inbound') {
            left -= 1;
            if (left === 0 && msgs[i].id) _unreadMarker.firstId = msgs[i].id;
        }
    }
    return _unreadMarker.firstId || null;
}

var MSG_GROUP_SECONDS = 180;
var MSG_EMOJI_ONLY = /^(?:\p{Extended_Pictographic}(?:️|‍\p{Extended_Pictographic}|\p{Emoji_Modifier})*\s*){1,3}$/u;

function _messageSameGroup(a, b) {
    return !!(a && b && a.direction === b.direction && !_messageIsNote(a) && !_messageIsNote(b) &&
        Math.abs(b.timestamp - a.timestamp) < MSG_GROUP_SECONDS &&
        RS.kit.time.date(a.timestamp) === RS.kit.time.date(b.timestamp));
}

// The name a reply quote shows: You, or the first name of who wrote the quoted message.
function _replyQuoteName(msg) {
    var quoted = msg.reply_to_id ? _conversationMessageById(msg.reply_to_id) : null;
    if (!quoted) return '';
    if (quoted.direction === 'outbound') return 'You';
    var info = _conversationNameInfo(lxmfActiveContact, null, false);
    return info.isHash ? info.name : String(info.name || '').trim().split(/\s+/)[0];
}

function _messageReactionsHtml(msg, ourHash) {
    var reactions = _msgReactions[msg.id] || [];
    if (!reactions.length) return '';
    var grouped = {};
    var order = [];
    reactions.forEach(function(r) {
        if (!grouped[r.emoji]) { grouped[r.emoji] = []; order.push(r.emoji); }
        grouped[r.emoji].push(r.sender);
    });
    return '<div class="msg-reacts">' + order.map(function(emoji) {
        var count = grouped[emoji].length;
        var isMine = grouped[emoji].indexOf(ourHash) !== -1;
        var label = (isMine ? 'Remove' : 'Add') + ' ' + emoji + ' reaction' + (count > 1 ? ', ' + count + ' reactions' : '');
        return '<button type="button" class="msg-react" data-emoji="' + escapeHtml(emoji) + '" data-msg-id="' + escapeHtml(msg.id) + '" ' +
            'aria-pressed="' + (isMine ? 'true' : 'false') + '" aria-label="' + escapeHtml(label) + '">' +
            '<span aria-hidden="true">' + escapeHtml(emoji) + '</span>' + (count > 1 ? '<span class="n" aria-hidden="true">' + count + '</span>' : '') + '</button>';
    }).join('') + '</div>';
}

// Pointer only: one "+" on the bubble's outer side that expands into React, Reply, and More.
function _messageHoverBarHtml(id) {
    var b = function(act, glyph, label, title, extra) {
        return '<button type="button" class="ui-icon-btn ui-icon-btn--sm" data-hover-act="' + act + '" aria-label="' + label + '" title="' + title + '"' + (extra || '') + '>' + RS.kit.icon(glyph) + '</button>';
    };
    return '<div class="msg-hover" role="group" aria-label="Message actions">' +
        '<button type="button" class="ui-icon-btn ui-icon-btn--sm msg-hover-plus" data-msg-id="' + id + '" aria-label="Message actions" title="Actions" aria-expanded="false">' + RS.kit.icon('plus') + '</button>' +
        '<div class="msg-hover-set" role="toolbar" aria-label="Message actions">' +
            b('react', 'smile-plus', 'Add a reaction', 'React', ' aria-haspopup="dialog" aria-expanded="false"') +
            b('reply', 'reply', 'Reply', 'Reply', ' tabindex="-1"') +
            b('more', 'more', 'More actions', 'More', ' tabindex="-1" aria-haspopup="dialog" aria-expanded="false"') +
        '</div></div>';
}

function _messageImageHtml(msg, overlay, mediaMeta) {
    var image = msg.image;
    var filename = image.filename || 'image';
    var mime = image.mime_type || image.mime || '';
    var sending = !!overlay;
    if (image.unavailable) {
        return '<div class="msg-file"><span class="ui-tile">' + RS.kit.icon('image') + '</span>' +
            '<span class="msg-file-text"><span class="msg-file-name">Attachment unavailable</span></span></div>';
    }
    var data = 'data-filename="' + escapeHtml(filename) + '" data-mime="' + escapeHtml(mime) + '"';
    var img;
    var stored = '';
    // stored_name -> async fetch; object_url/data_url -> local optimistic preview.
    if (image.stored_name) {
        stored = ' data-stored-name="' + escapeHtml(image.stored_name) + '"';
        img = '<img' + stored + ' alt="Photo" ' + data + ' class="msg-img rs-lazy-image">';
    } else if (image.object_url || image.data_url) {
        img = '<img src="' + escapeHtml(image.object_url || image.data_url) + '" alt="Photo" class="msg-img" ' + data + '>';
    } else {
        return '';
    }
    return '<div class="msg-image' + (sending ? ' is-sending' : '') + '">' +
        '<button type="button" class="msg-image-btn" aria-label="Open image"' + stored + ' ' + data + '>' + img + '</button>' +
        (overlay || '') + (mediaMeta || '') + '</div>';
}

function _messageFilesHtml(msg, sending) {
    return (msg.attachments || []).map(function(att) {
        var name = att.unavailable ? 'Attachment unavailable' : (att.filename || 'file');
        var save = !att.unavailable && att.stored_name && !sending
            ? '<button type="button" class="ui-icon-btn ui-icon-btn--sm msg-file-save" data-stored-name="' + escapeHtml(att.stored_name) + '" ' +
                'aria-label="Save ' + escapeHtml(name) + '" title="Save">' + RS.kit.icon('download') + '</button>'
            : '';
        return '<div class="msg-file"><span class="ui-tile">' + RS.kit.icon('file') + '</span>' +
            '<span class="msg-file-text"><span class="msg-file-name">' + escapeHtml(name) + '</span>' +
            (att.size ? '<span class="msg-file-size">' + escapeHtml(prettySize(att.size)) + '</span>' : '') + '</span>' + save + '</div>';
    }).join('');
}

// One message: bubble (tinted when yours), time and mark on the last line, reactions under it.
function _messageRowHtml(msg, prev, next, ctx) {
    var isOut = msg.direction === 'outbound';
    var id = escapeHtml(msg.id || '');
    var gPrev = _messageSameGroup(prev, msg) && !(ctx.newlineId && msg.id === ctx.newlineId);
    var gNext = _messageSameGroup(msg, next) && !(ctx.newlineId && next && next.id === ctx.newlineId);
    var pos = gPrev && gNext ? ' msg-g-mid' : gPrev ? ' msg-g-last' : gNext ? ' msg-g-first' : '';
    var paper = _messageIsPaper(msg);
    var mark = paper ? _messagePaperChipHtml(msg, isOut) : (isOut ? _messageStateIconHtml(msg) : '');
    var progressPercent = isOut ? _messageProgressPercent(msg) : null;
    var canCancelSend = isOut && _messageCanCancelSend(msg);
    var canCancelTransfer = isOut && _messageCanCancelTransfer(msg);
    var kind = isOut && !paper ? _messageMarkKind(msg.state) : '';
    // Earlier messages in a group keep a mark that needs you; settled ones show only on the last.
    var exceptional = paper || kind === 'waiting' || kind === 'failed' || kind === 'sending' || kind === 'cancelled' || kind === 'rejected';

    var audioHtml = msg.audio && window.RS && RS.voiceMemos ? RS.voiceMemos.renderAudio(msg.audio, msg) : '';
    var body = _messageBodyHtml(msg, !!audioHtml);
    var plain = _messageDisplayContent(msg, !!audioHtml);
    var hasText = !!body.html;
    var hasFiles = !!(msg.attachments && msg.attachments.length);
    var imageOnly = !!msg.image && !hasText && !hasFiles && !audioHtml;
    var emoji = hasText && !body.markdown && !msg.image && !hasFiles && !audioHtml && MSG_EMOJI_ONLY.test(String(plain).trim());
    var showMeta = !gNext || exceptional || !!msg.image || !!audioHtml;
    var imageCancels = canCancelTransfer && !!msg.image;
    var meta = showMeta
        ? '<span class="msg-meta">' +
            (canCancelSend && !imageCancels ? '' : '<time class="msg-clock" title="' + escapeHtml(RS.kit.time.full(msg.timestamp)) + '">' + escapeHtml(RS.kit.time.clock(msg.timestamp)) + '</time>') +
            _messageProgressMetaHtml(msg) + (canCancelSend && !imageCancels ? _messageInlineCancelHtml(msg) : '') + mark + '</span>'
        : '';

    var parts = '';
    if (msg.reply_to_id || msg.reply_to_preview) {
        var quoteName = _replyQuoteName(msg);
        parts += '<button type="button" class="msg-quote" data-reply-id="' + escapeHtml(msg.reply_to_id || '') + '" ' +
            'aria-label="' + escapeHtml(quoteName ? 'Go to the message from ' + quoteName : 'Go to the quoted message') + '">' +
            (quoteName ? '<b>' + escapeHtml(quoteName) + '</b>' : '') + '<span>' + escapeHtml(msg.reply_to_preview || '') + '</span></button>';
    }
    if (msg.image) {
        var overlay = imageCancels ? _messageSendCancelOverlayHtml(msg, progressPercent) : '';
        var mediaMeta = imageOnly && !imageCancels && meta ? meta.replace('class="msg-meta"', 'class="msg-meta msg-meta--media"') : '';
        parts += _messageImageHtml(msg, overlay, mediaMeta);
    }
    if (hasFiles) parts += _messageFilesHtml(msg, canCancelTransfer);
    if (audioHtml) parts += audioHtml;
    var textMeta = hasText && !audioHtml && !body.markdown ? meta : '';
    if (hasText) {
        parts += '<div class="msg-text' + (body.markdown ? ' md' : '') + (emoji ? ' msg-text--emoji' : '') + '">' + body.html + textMeta + '</div>';
    }
    if (!imageOnly && !textMeta && meta) parts += '<div class="msg-meta-row">' + meta + '</div>';

    var bubbleClass = 'msg-bubble' + (imageOnly ? ' msg-bubble--media' : '') + (msg.image && !imageOnly ? ' msg-bubble--image' : '') +
        (emoji ? ' msg-bubble--emoji' : '') + (audioHtml ? ' msg-bubble--voice' : '');
    return '<div class="msg-row msg-row--' + (isOut ? 'out' : 'in') + pos + '">' +
        '<div class="msg-col"><div class="' + bubbleClass + '" data-msg-id="' + id + '"' + (ctx.pointer ? ' tabindex="0"' : '') + '>' +
            parts + (ctx.pointer ? _messageHoverBarHtml(id) : '') + '</div>' +
            _messageReactionsHtml(msg, ctx.ourHash) + '</div></div>';
}

function renderConversation(options) {
    options = options || {};
    var container = document.getElementById('lxmf-messages');
    if (!container) return;
    if (_deferActiveMessageInteractionRender(options)) return;
    var activeOwnerHash = _activeContextMenu && _activeContextMenu.ownerHash;
    if (activeOwnerHash && activeOwnerHash !== _canonicalConversationHash(lxmfActiveContact)) {
        _cancelScheduledDeferredConversationRender();
        _clearDeferredConversationRender();
    }
    options = _takeDeferredConversationRenderOptions(options);
    var hoverOpen = _openHoverMessageId(container);
    _dismissContextMenu({ restoreFocus: false, flushDeferredRender: false });
    _detachMessageLongPressHandlers();
    RS.chatScroll.wire(container);
    _wireTranscript(container);
    var scrollState = RS.chatScroll.capture(container);
    var renderedImages = _captureLxmfRenderedImages(container);

    var open = !!lxmfActiveContact;
    var composer = document.getElementById('lxmf-composer');
    if (composer) composer.hidden = !open;
    var top = document.getElementById('lxmf-chat-top');
    if (top) top.hidden = !open;
    var chatArea = document.getElementById('lxmf-chat-area');
    if (chatArea) chatArea.classList.toggle('has-conversation', open);
    if (open) {
        var header = document.getElementById('lxmf-chat-header');
        if (header) header.style.display = '';
        _renderChatHeaderName();
        _applyChatHeaderPresence();
    }
    if (RS.threads) RS.threads.render();
    if (RS.rsnode) RS.rsnode.render();
    renderVoiceUi();

    if (!open) {
        container._rsNoConversationHtml = null;
        _renderFirstRunDetail();
        _syncJumpButton(container);
        return;
    }
    container._rsNoConversationHtml = null;

    if (lxmfConversation.length === 0) {
        container.innerHTML = _conversationIntroHtml(lxmfActiveContact);
        _syncJumpButton(container);
        return;
    }

    lxmfConversation.forEach(function(msg) {
        if (msg.reactions) _msgReactions[msg.id] = msg.reactions;
    });

    var msgs = lxmfConversation;
    var ctx = {
        ourHash: lxmfIdentity ? (lxmfIdentity.hash || '') : '',
        pointer: !_mobileMessageActionsUseLongPress(),
        newlineId: _unreadMarkerFor(msgs)
    };
    var htmlParts = [];
    var lastDate = '';
    for (var mi = 0; mi < msgs.length; mi++) {
        var msg = msgs[mi];
        var date = RS.kit.time.date(msg.timestamp);
        if (date !== lastDate) {
            htmlParts.push('<p class="ui-caption msg-day"><time title="' + escapeHtml(RS.kit.time.full(msg.timestamp)) + '">' + escapeHtml(date) + '</time></p>');
            lastDate = date;
        }
        if (ctx.newlineId && msg.id === ctx.newlineId) {
            htmlParts.push('<div class="msg-newline" role="separator"><span>New messages</span></div>');
        }
        if (_messageIsNote(msg)) {
            htmlParts.push('<div class="msg-note" data-msg-id="' + escapeHtml(msg.id || '') + '"><span>' +
                escapeHtml(msg.content || '') + '</span><time>' + escapeHtml(RS.kit.time.clock(msg.timestamp)) + '</time></div>');
            continue;
        }
        htmlParts.push(_messageRowHtml(msg, msgs[mi - 1], msgs[mi + 1], ctx));
    }
    container.innerHTML = htmlParts.join('');
    // Preserve decoded images before restoring scroll. Replacing them with
    // empty placeholders temporarily shrinks the transcript on every progress
    // event and can clamp its scroll offset to a much earlier message.
    _restoreLxmfRenderedImages(container, renderedImages);
    if (window.RS && RS.voiceMemos && typeof RS.voiceMemos.hydratePlayers === 'function') {
        RS.voiceMemos.hydratePlayers(container);
    }
    var shouldPinMessages;
    var line = container.querySelector('.msg-newline');
    if (line && _unreadMarker && !_unreadMarker.placed && options.forceScrollBottom) {
        // The chat opens at the first unread message when the new ones fill more than the view.
        _unreadMarker.placed = true;
        var target = Math.max(0, line.offsetTop - 16);
        if (target + container.clientHeight < container.scrollHeight - 1) {
            RS.chatScroll.restore(container, { scrollTop: target, followLatest: false });
            shouldPinMessages = false;
        }
    }
    if (shouldPinMessages === undefined) shouldPinMessages = RS.chatScroll.applyAfterRender(container, scrollState, options);
    RS.chatScroll.watchImages(container, shouldPinMessages);
    if (shouldPinMessages) _jumpNewIds = [];

    // Swap cached images immediately; hydrate uncached media only when it is
    // near the viewport. Fetches are single-flight and globally bounded.
    container.querySelectorAll('img.rs-lazy-image[data-stored-name]').forEach(function(img) {
        var name = img.getAttribute('data-stored-name');
        if (!name) return;
        var cachedUrl = _getImageBlobUrl(name);
        if (cachedUrl) {
            var cachedFile = _getImageDownloadFile(name);
            if (cachedFile) {
                if (cachedFile.filename) img.setAttribute('data-filename', cachedFile.filename);
                if (cachedFile.mime) img.setAttribute('data-mime', cachedFile.mime);
            }
            if (img.getAttribute('src') !== cachedUrl) img.src = cachedUrl;
            return;
        }
        _observeImageHydration(img, name);
    });

    _bindMessageSpoilers(container);
    _bindMessageCodeBlocks(container);
    var pointer = document.documentElement.dataset.inputModality !== 'touch';
    container.querySelectorAll('.rs-message-link.md-link[data-url]').forEach(function(link) {
        if (!pointer) return;
        var linkTarget = rsLinkTarget(link.getAttribute('data-url'));
        if (linkTarget) {
            var p = linkTarget.parts;
            link.title = p.scheme + p.www + p.host + p.port + p.rest;
        }
    });

    container.querySelectorAll('.rs-message-link[data-url]').forEach(function(link) {
        // Middle-click must not bypass the warning or autoscroll.
        link.addEventListener('mousedown', function(e) {
            if (e.button === 1) e.preventDefault();
        });
        link.addEventListener('auxclick', function(e) {
            if (e.button !== 1) return;
            e.preventDefault();
            e.stopPropagation();
            if (_messageLinkActivationAllowed(this) && !this.closest('.md-spoiler:not(.is-revealed)')) {
                _confirmExternalLink(this.getAttribute('data-url'), this);
            }
        });
        link.addEventListener('click', function(e) {
            e.preventDefault();
            e.stopPropagation();
            if (_consumePendingMessageHoldActivation(e, this)) return;
            if (!_messageLinkActivationAllowed(this)) return;
            var covered = this.closest('.md-spoiler:not(.is-revealed)');
            if (covered) {
                _revealMessageSpoiler(covered);
                return;
            }
            var url = this.getAttribute('data-url');
            if (url) _confirmExternalLink(url, this);
        });
    });

    container.querySelectorAll('.msg-cancel').forEach(function(btn) {
        _bindMessageFocusPreservingActivation(btn, function() {
            _cancelLxmfSend(btn.getAttribute('data-msg-id'));
        });
    });

    container.querySelectorAll('.msg-react').forEach(function(pill) {
        _bindMessageFocusPreservingActivation(pill, function(e) {
            var emoji = this.getAttribute('data-emoji');
            var msgId = this.getAttribute('data-msg-id');
            var msgData = lxmfConversation.find(function(m) { return m.id === msgId; }) || { id: msgId };
            _sendReactionForMessage(msgData, emoji, {
                dismiss: false,
                restoreReactionFocus: true,
                focusExpected: _messageActivationExpectsFocus(e),
            });
        }.bind(pill));
    });

    container.querySelectorAll('button.msg-mark').forEach(function(markBtn) {
        _bindMessageFocusPreservingActivation(markBtn, function(event) {
            if (_consumePendingMessageHoldActivation(event, markBtn)) return;
            var msgId = markBtn.getAttribute('data-msg-id');
            var msgData = lxmfConversation.find(function(m) { return m.id === msgId; });
            if (!msgData) return;
            if (markBtn.classList.contains('msg-mark--paper')) {
                _openPaperMessageShare(msgData);
                return;
            }
            if (_activeContextMenu && _activeContextMenu.trigger === markBtn) {
                _dismissContextMenu();
                return;
            }
            var bubble = markBtn.closest('.msg-bubble');
            var rect = markBtn.getBoundingClientRect();
            _showMsgContextMenu(msgData, rect.left + (rect.width / 2), rect.bottom, bubble, markBtn, {
                focusDialog: _messageActivationExpectsFocus(event),
                anchor: markBtn,
            });
        });
    });

    container.querySelectorAll('.msg-bubble').forEach(function(bubble) {
        bubble.addEventListener('mousedown', function(e) {
            if (e.button !== 2) return;
            _rememberMessagePointerContextSelection(e, this);
        });
        if (window.RS && RS.gestures && typeof RS.gestures.attachLongPress === 'function') {
            var detachLongPress = RS.gestures.attachLongPress(bubble, {
                duration: 500,
                moveCancelPx: 12,
                excludeZone: function(touch) {
                    return _messageElevatedTextStartsNativeSelection(touch, bubble) ||
                        _messageTouchStartsDirectControl(touch, bubble);
                },
                hapticStages: [{ at: 0.55, level: 'light' }],
                // The first hold owns message actions. While that same bubble
                // remains elevated, a second hold on its text is left entirely
                // to the platform's native selection gesture.
                onFire: function(touch) {
                    if (_messageElevatedTextStartsNativeSelection(touch, bubble) ||
                            _messageSelectionIntersectsBubble(bubble)) return;
                    var msgId = bubble.getAttribute('data-msg-id');
                    if (!msgId) return;
                    var msgData = lxmfConversation.find(function(m) { return m.id === msgId; });
                    if (!msgData) return;
                    _armPendingMessageHoldActivation(bubble, msgId, touch);
                    _showMsgContextMenu(msgData, touch.clientX, touch.clientY, bubble, null, {
                        linkUrl: _messageLinkUrlAt(touch.target),
                        sheet: true,
                    });
                }
            });
            if (typeof detachLongPress === 'function') _messageLongPressDetachFns.push(detachLongPress);
        }
        bubble.addEventListener('touchend', function() {
            _releasePendingMessageHoldActivation(this);
        });
        bubble.addEventListener('touchcancel', function() {
            if (_pendingMessageHoldActivation && _pendingMessageHoldActivation.bubble === this) {
                _clearPendingMessageHoldActivation(_pendingMessageHoldActivation.sequence);
            }
        });
        bubble.addEventListener('contextmenu', function(e) {
            var target = e.target;
            var selectionExistedBeforePointer = _consumeMessagePointerContextSelection(this);
            var disposition = _messageContextMenuDisposition(
                target,
                this,
                undefined,
                selectionExistedBeforePointer
            );
            if (disposition === 'native') return;
            e.preventDefault();
            if (disposition === 'suppress') return;
            if (selectionExistedBeforePointer === false) _clearNativeMessageSelection();
            var msgId = this.getAttribute('data-msg-id');
            if (!msgId) return;
            var msgData = lxmfConversation.find(function(m) { return m.id === msgId; });
            if (msgData) {
                var focusDialog = document.documentElement.dataset.inputModality !== 'touch';
                _showMsgContextMenu(msgData, e.clientX, e.clientY, this, focusDialog ? this : null, {
                    focusDialog: focusDialog,
                    linkUrl: _messageLinkUrlAt(target),
                });
            }
        });
    });

    _applyMessageSelection();
    if (hoverOpen) _reopenHoverBar(container, hoverOpen);
    _syncJumpButton(container);
}

// Must match backend out_<uuid> format so the round-trip preserves msg_id.
function generateMsgId() {
    var hex = '';
    for (var i = 0; i < 32; i++) {
        hex += Math.floor(Math.random() * 16).toString(16);
    }
    return 'out_' + hex;
}

function _captureLxmfSendFocusState() {
    var input = document.getElementById('lxmf-input');
    if (RS.composer) RS.composer.captureFocus(input);
}

function _consumeLxmfSendFocusState(input) {
    return RS.composer ? RS.composer.consumeFocus(input) : document.activeElement === input;
}

function _focusLxmfComposerInput(input) {
    if (RS.composer) {
        RS.composer.focusWithoutScroll(input);
        return;
    }
    try { input.focus({ preventScroll: true }); }
    catch (_) { input.focus(); }
}

function _isLxmfComposerFocused() {
    var input = document.getElementById('lxmf-input');
    return !!(input && document.activeElement === input);
}

function _shouldPreserveLxmfComposerKeyboard() {
    return (typeof isMobile === 'function' ? isMobile() : window.innerWidth <= 768) && _isLxmfComposerFocused();
}

function _restoreLxmfComposerKeyboard(shouldRestore) {
    if (!shouldRestore) return;
    var input = document.getElementById('lxmf-input');
    if (!input || document.activeElement === input) return;
    setTimeout(function() {
        if (document.body.classList.contains('view-chat-detail') || lxmfActiveContact) {
            _focusLxmfComposerInput(input);
        }
    }, 0);
}

function _finishLxmfComposerSend(input, shouldRestoreFocus, targetHash) {
    targetHash = _canonicalConversationHash(targetHash || lxmfActiveContact);
    if (targetHash !== _canonicalConversationHash(lxmfActiveContact)) return;
    input.value = '';
    input.style.height = '';
    input.scrollTop = 0;
    delete _lxmfDrafts[_activeConversationKey()];
    if (shouldRestoreFocus) {
        _focusLxmfComposerInput(input);
    } else if (document.activeElement === input) {
        input.blur();
    }
    var counter = document.getElementById('lxmf-char-count');
    if (counter) { counter.textContent = ''; counter.classList.remove('is-over'); counter.hidden = true; }
    if (window.RS && RS.voiceMemos) RS.voiceMemos.syncComposer();
}

function _markOptimisticMessageFailed(clientMsgId, error, targetHash, thread) {
    targetHash = _canonicalConversationHash(targetHash || lxmfActiveContact);
    var target = _conversationMessagesFor(targetHash, thread);
    for (var i = 0; i < target.messages.length; i++) {
        if (target.messages[i] && target.messages[i].id === clientMsgId) {
            target.messages[i].state = 'failed';
            target.messages[i].failure_reason = (error && (error.code || error.message)) || 'attachment_failed';
            break;
        }
    }
    _commitConversationMessages(targetHash, target);
    if (target.active) renderConversation();
    if (typeof showToast === 'function') {
        showToast((error && error.message) || 'Attachment could not be sent', 'toast-error', 4500);
    }
}

function _staleConversationOperationError(message) {
    var error = new Error(message || 'This operation no longer belongs to the active conversation.');
    error.code = 'stale_conversation_owner';
    error.stale_owner = true;
    return error;
}

function _cancelStagedAttachmentToken(stageToken) {
    if (!stageToken) return Promise.resolve(false);
    return RS.invoke('cancel_attachment_stage', { token: stageToken }).then(function() {
        return true;
    }).catch(function() {
        return false;
    });
}

var COMPOSER_FORMAT_KEYS = { b: '**', i: '*', e: '`' };

function _composerFormatMarker(e) {
    if (!(window.RS && RS.markdown) || e.altKey || e.isComposing) return null;
    var mod = /Mac|iPhone|iPad/.test(navigator.platform || '') ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
    if (!mod) return null;
    var key = (e.key || '').toLowerCase();
    if (e.shiftKey) return key === 'x' ? '~~' : null;
    return COMPOSER_FORMAT_KEYS[key] || null;
}

// Formatting sets LXMF FIELD_RENDERER = Markdown; plain text goes out untouched.
function _prepareComposedMessage(text) {
    if (!text || !(window.RS && RS.markdown)) return { content: text, renderer: null, preview: text };
    var prepared = RS.markdown.prepare(text, { links: _messageLinkRanges });
    if (!prepared.markdown) return { content: text, renderer: null, preview: text };
    return {
        content: prepared.content,
        renderer: RS.markdown.RENDERER,
        preview: RS.markdown.strip(prepared.content),
    };
}

function sendLxmfMessage(deliveryMethod, shareContext) {
    if (!lxmfActiveContact) return;
    if (!shareContext && RS.textShares && RS.textShares.interceptSend(deliveryMethod)) return;
    var sendOwner = _conversationOwnerSnapshot();
    var targetHash = sendOwner.hash;
    var input = document.getElementById('lxmf-input');
    if (!input) return;
    var shouldRestoreComposerFocus = _consumeLxmfSendFocusState(input);
    // A node reads commands as plain text.
    var composed = RS.rsnode && RS.rsnode.isDevice(targetHash)
        ? { content: input.value.trim(), renderer: null, preview: input.value.trim() }
        : _prepareComposedMessage(input.value.trim());
    var text = composed.content;
    var renderer = composed.renderer;
    var chosenDelivery = _deliveryPrefOrAuto(deliveryMethod);
    var maxMessageBytes = lxmfLimits.max_message_bytes || 134217727;
    if (text && _utf8ByteLength(text) > maxMessageBytes) {
        showToast('Message is too large for this network (' + prettySize(_utf8ByteLength(text)) + ' > ' + prettySize(maxMessageBytes) + ')', 'toast-warning', 5000);
        return;
    }

    if (lxmfPendingFile) {
        var pendingAttachment = lxmfPendingFile;
        if (_canonicalConversationHash(pendingAttachment.destination) !== targetHash) {
            clearPendingFile();
            showToast('Attach the file again for this conversation', 'toast-warning', 3000);
            return;
        }
        if (pendingAttachment.preparing) {
            showToast('Choose an image size before sending', 'toast-warning', 2600);
            return;
        }
        var attachMsgId = generateMsgId();
        var isImage = pendingAttachment.inline_image === true;
        var optimisticImageUrl = isImage ? pendingAttachment.preview_url : null;
        pendingAttachment.stage_promise.then(function(stageToken) {
            if (!stageToken) throw pendingAttachment.stage_error || new Error('Attachment staging failed');
            if (!_conversationOwnerIsCurrent(sendOwner)) {
                return _cancelStagedAttachmentToken(stageToken).then(function() {
                    throw _staleConversationOperationError('Attachment was not sent after changing conversations.');
                });
            }
            pendingAttachment.staging_token = null;
            return RS.invoke('send_lxmf_with_staged_attachment', {
                args: {
                    dest_hash: targetHash,
                    content: text,
                    delivery_method: chosenDelivery,
                    client_msg_id: attachMsgId,
                    staging_token: stageToken,
                    renderer: renderer,
                    thread: sendOwner.thread || null,
                }
            });
        }).then(function(resp) {
            _handleLxmfSendAccepted(resp, attachMsgId, targetHash, sendOwner);
            if (shareContext && RS.textShares) {
                if (resp && resp.msg_id && !resp.cancelled) RS.textShares.accepted(shareContext);
                else RS.textShares.failed(shareContext);
            }
        }).catch(function(error) {
            if (shareContext && RS.textShares) RS.textShares.failed(shareContext);
            if (_conversationOwnerIdentityIsCurrent(sendOwner)) {
                _markOptimisticMessageFailed(attachMsgId, error, targetHash, sendOwner.thread);
            }
        }).finally(function() {
            if (optimisticImageUrl) {
                setTimeout(function() {
                    try { URL.revokeObjectURL(optimisticImageUrl); } catch (_) {}
                }, 60000);
            }
        });

        var attachmentWasActive = _appendConversationMessage(targetHash, {
            id: attachMsgId,
            thread: sendOwner.thread,
            direction: 'outbound',
            content: text,
            renderer: renderer,
            timestamp: Date.now() / 1000,
            state: 'sending',
            delivery_method: _optimisticDeliveryMethod(chosenDelivery),
            attachments: isImage ? [] : [{ filename: pendingAttachment.name, size: pendingAttachment.size }],
            image: isImage ? {
                mime_type: pendingAttachment.mime,
                size: pendingAttachment.size,
                filename: pendingAttachment.name,
                object_url: pendingAttachment.preview_url,
            } : null,
        });

        // Capture before clearPendingFile() wipes pending state.
        var attachPreview = composed.preview || (isImage ? 'Photo' : pendingAttachment.name);
        pendingAttachment.preview_url = null;
        pendingAttachment.detached_for_send = true;
        clearPendingFile();
        if (attachmentWasActive) renderConversation({ forceScrollBottom: true });
        _updateConversationPreview(targetHash, attachPreview, Date.now() / 1000);
        loadConversations();
        _finishLxmfComposerSend(input, shouldRestoreComposerFocus, targetHash);
        if (shareContext && RS.textShares) RS.textShares.dispatched(shareContext);
        return;
    }

    if (!text) return;

    var msgId = generateMsgId();

    if (_replyTarget) {
        RS.invoke('send_lxmf_reply', {
            args: {
                dest_hash: targetHash,
                content: text,
                delivery_method: chosenDelivery,
                reply_to_id: _replyTarget.id,
                reply_to_preview: _replyTarget.content,
                client_msg_id: msgId,
                renderer: renderer,
                thread: sendOwner.thread || null,
            }
        }).then(function(resp) {
            _handleLxmfSendAccepted(resp, msgId, targetHash, sendOwner);
        }).catch(function() {});
        var replyWasActive = _appendConversationMessage(targetHash, {
            id: msgId,
            thread: sendOwner.thread,
            direction: 'outbound',
            content: text,
            renderer: renderer,
            timestamp: Date.now() / 1000,
            state: 'sending',
            delivery_method: _optimisticDeliveryMethod(chosenDelivery),
            reply_to_id: _replyTarget.id,
            reply_to_preview: _replyTarget.content,
        });
        clearReplyTarget();
        if (replyWasActive) renderConversation({ forceScrollBottom: true });
        _updateConversationPreview(targetHash, composed.preview, Date.now() / 1000);
        loadConversations();
        _finishLxmfComposerSend(input, shouldRestoreComposerFocus, targetHash);
        return;
    }

    RS.invoke('send_lxmf_message', {
        args: {
            dest_hash: targetHash,
            content: text,
            delivery_method: chosenDelivery,
            client_msg_id: msgId,
            renderer: renderer,
            thread: sendOwner.thread || null,
        }
    }).then(function(resp) {
        _handleLxmfSendAccepted(resp, msgId, targetHash, sendOwner);
        if (shareContext && RS.textShares) {
            if (resp && resp.msg_id && !resp.cancelled) RS.textShares.accepted(shareContext);
            else RS.textShares.failed(shareContext);
        }
    }).catch(function() {
        if (shareContext && RS.textShares) RS.textShares.failed(shareContext);
    });
    if (shareContext && RS.textShares) RS.textShares.dispatched(shareContext);

    var messageWasActive = _appendConversationMessage(targetHash, {
        id: msgId,
        thread: sendOwner.thread,
        direction: 'outbound',
        content: text,
        renderer: renderer,
        timestamp: Date.now() / 1000,
        state: 'sending',
        delivery_method: _optimisticDeliveryMethod(chosenDelivery),
    });
    if (messageWasActive) renderConversation({ forceScrollBottom: true });
    _updateConversationPreview(targetHash, composed.preview, Date.now() / 1000);
    loadConversations();
    _finishLxmfComposerSend(input, shouldRestoreComposerFocus, targetHash);
}

function _paperUriForMessage(msg) {
    if (msg && msg.paper_uri) return Promise.resolve(msg.paper_uri);
    if (!msg || !msg.id) return Promise.reject(new Error('Paper link unavailable'));
    return RS.invoke('get_paper_uri', { message_id: msg.id }).then(function(resp) {
        var uri = resp && resp.paper_uri;
        if (!uri) throw new Error('Paper link unavailable');
        msg.paper_uri = uri;
        return uri;
    });
}

// Reopens the stored link; regenerating would re-encrypt and defeat dedupe.
function _openPaperMessageShare(msg) {
    if (!(window.RS && RS.qr && typeof RS.qr.openPaperShare === 'function')) return;
    var hash = _canonicalConversationHash(lxmfActiveContact);
    RS.qr.openPaperShare({
        recipientHash: hash,
        recipientName: _conversationNameInfo(hash, null, false).name,
        uri: msg.paper_uri || null,
        loadUri: function() { return _paperUriForMessage(msg); },
    });
}

var _paperSendInFlight = false;

function sendLxmfPaperMessage() {
    if (!lxmfActiveContact || _paperSendInFlight) return;
    var input = document.getElementById('lxmf-input');
    if (!input) return;
    var sendOwner = _conversationOwnerSnapshot();
    var targetHash = sendOwner.hash;
    var text = input.value.trim();
    if (!text) return;
    if (lxmfPendingFile) {
        showToast('Paper messages can\'t include attachments.', 'toast-warning', 4000);
        return;
    }
    _paperSendInFlight = true;
    RS.invoke('create_paper_message', {
        dest_hash: targetHash,
        content: text,
        title: '',
        thread: sendOwner.thread || null,
    }).then(function(resp) {
        if (!_conversationOwnerIdentityIsCurrent(sendOwner)) return;
        var uri = resp && resp.paper_uri;
        if (!uri) throw new Error('Paper link unavailable');
        var message = Object.assign({
            id: generateMsgId(),
            direction: 'outbound',
            content: text,
            timestamp: Date.now() / 1000,
        }, (resp && resp.message) || {});
        message.state = 'paper';
        message.delivery_method = 'paper';
        message.paper_uri = message.paper_uri || uri;
        message.thread = _canonicalConversationThread(message.thread || sendOwner.thread);
        var target = _conversationMessagesFor(targetHash, message.thread);
        var existing = target.messages.find(function(m) { return m.id === message.id; });
        if (existing && !existing.paper_uri) existing.paper_uri = message.paper_uri;
        var wasActive = existing ? target.active : _appendConversationMessage(targetHash, message);
        if (wasActive) renderConversation({ forceScrollBottom: true });
        _updateConversationPreview(targetHash, text, message.timestamp);
        loadConversations();
        if (_conversationOwnerIsCurrent(sendOwner) && input.value.trim() === text) {
            if (_replyTarget) clearReplyTarget();
            _finishLxmfComposerSend(input, false, targetHash);
        }
        if (RS.qr && typeof RS.qr.openPaperShare === 'function') {
            RS.qr.openPaperShare({
                recipientHash: targetHash,
                recipientName: _conversationNameInfo(targetHash, null, false).name,
                uri: message.paper_uri,
            });
        }
    }).catch(function(err) {
        var code = typeof paperErrorCode === 'function' ? paperErrorCode(err) : (err && err.code);
        if (code === 'locked') return;
        if (code === 'paper_too_long') {
            var overage = typeof paperErrorDetail === 'function' ? paperErrorDetail(err, 'overage_chars') : null;
            showToast(overage
                ? 'Too long for a paper message. Remove about ' + Number(overage).toLocaleString('en-US') + ' characters.'
                : 'Too long for a paper message.', 'toast-warning', 4000);
        } else if (code === 'paper_unknown_identity') {
            showToast('Can\'t create a paper message until ' + _conversationNameInfo(targetHash, null, false).name + ' announces.',
                'toast-warning', 4000);
        } else {
            var message = err && err.message && !/\bpaper_[a-z_]+\b/.test(err.message) ? err.message : '';
            showToast(message || 'Couldn\'t create the paper message', 'toast-error', 4000);
        }
    }).then(function() {
        _paperSendInFlight = false;
    });
}

function sendLxmfVoiceMemo(voiceDraft, targetHash, options) {
    options = options || {};
    targetHash = _canonicalConversationHash(targetHash);
    var sendOwner = options.owner || _conversationOwnerSnapshot();
    var stageToken = String(voiceDraft && voiceDraft.staging_token || '');
    if (!targetHash || targetHash !== _canonicalConversationHash(sendOwner.hash) ||
        !_conversationOwnerIsCurrent(sendOwner) || !voiceDraft || !stageToken) {
        if (!stageToken) {
            return Promise.reject(new Error('Voice message staging is unavailable.'));
        }
        return _cancelStagedAttachmentToken(stageToken).then(function() {
            throw _staleConversationOperationError('Voice message was not sent after changing conversations.');
        });
    }
    var input = document.getElementById('lxmf-input');
    if (!input) {
        return _cancelStagedAttachmentToken(stageToken).then(function() {
            throw new Error('Message composer is unavailable.');
        });
    }
    var msgId = generateMsgId();
    var chosenDelivery = _deliveryPrefOrAuto('auto');
    if (!_conversationOwnerIsCurrent(sendOwner) ||
        (typeof options.isCurrent === 'function' && !options.isCurrent())) {
        return _cancelStagedAttachmentToken(stageToken).then(function() {
            throw _staleConversationOperationError('Voice message was not sent after changing conversations.');
        });
    }
    if (typeof options.onAdmissionStart === 'function') options.onAdmissionStart();
    return RS.invoke('send_lxmf_voice_message', {
        args: {
            dest_hash: targetHash,
            delivery_method: chosenDelivery,
            client_msg_id: msgId,
            staging_token: stageToken,
            thread: sendOwner.thread || null,
        }
    }).catch(function(error) {
        // Only native confirmation of no admission and successful restoration
        // allows reuse. A stale conversation still owns no retained recording.
        if (error && error.code === 'voice_retryable' && _conversationOwnerIsCurrent(sendOwner) &&
            (typeof options.isCurrent !== 'function' || options.isCurrent())) throw error;
        return _cancelStagedAttachmentToken(stageToken).then(function() { throw error; });
    }).then(function(resp) {
        if (!_conversationOwnerIdentityIsCurrent(sendOwner) || (resp && resp.cancelled)) return resp;
        var acceptedId = (resp && resp.msg_id) || msgId;
        if (window.RS && RS.voiceMemos) RS.voiceMemos.registerDraft(acceptedId, voiceDraft);
        var isActive = _appendConversationMessage(targetHash, {
            id: acceptedId,
            thread: sendOwner.thread,
            direction: 'outbound',
            content: 'Voice message',
            timestamp: Date.now() / 1000,
            state: resp && resp.cancelled ? 'cancelled' : 'sending',
            delivery_method: _optimisticDeliveryMethod(chosenDelivery),
            audio: {
                mode: voiceDraft.audio_mode == null ? 0x10 : voiceDraft.audio_mode,
                supported: true,
                stored_name: '',
                size: voiceDraft.size || 0,
                voice_memo_key: acceptedId,
                voice_memo: {
                    duration_ms: voiceDraft.duration_ms,
                    waveform: voiceDraft.waveform || [],
                },
            },
        });
        if (isActive) renderConversation({ forceScrollBottom: true });
        _updateConversationPreview(targetHash, 'Voice message', Date.now() / 1000);
        loadConversations();
        _finishLxmfComposerSend(input, false, targetHash);
        return resp;
    });
}

window.sendLxmfVoiceMemo = sendLxmfVoiceMemo;

function triggerFileAttachment() {
    var fileInput = document.getElementById('lxmf-file-input');
    if (fileInput) fileInput.click();
}

function _ensureAttachmentMediaPermission(opts) {
    opts = opts || {};
    if (!window.RS || !RS.mediaPermissions || typeof RS.mediaPermissions.ensure !== 'function') {
        return Promise.resolve(true);
    }
    return RS.mediaPermissions.ensure(opts).then(function(granted) {
        if (!granted) {
            var message = opts.audio
                ? 'Camera or microphone permission denied'
                : 'Camera permission denied';
            showToast(message, 'toast-warning', 3500);
        }
        return granted;
    });
}

function triggerPhotosAttachment() {
    var input = document.getElementById('lxmf-photos-input');
    if (input) input.click();
}

function triggerCameraAttachment() {
    var input = document.getElementById('lxmf-camera-input');
    if (!input) return;
    _ensureAttachmentMediaPermission({ camera: true }).then(function(granted) {
        if (granted) input.click();
    });
}

function triggerVideoAttachment() {
    var input = document.getElementById('lxmf-video-input');
    if (!input) return;
    _ensureAttachmentMediaPermission({ camera: true, audio: true }).then(function(granted) {
        if (granted) input.click();
    });
}

function _pendingAttachmentName(file) {
    return file && file.name ? file.name : 'Photo';
}

function _readBlobBase64(blob) {
    return new Promise(function(resolve, reject) {
        var reader = new FileReader();
        reader.onload = function(e) {
            var result = String((e && e.target && e.target.result) || '');
            var comma = result.indexOf(',');
            resolve(comma >= 0 ? result.slice(comma + 1) : result);
        };
        reader.onerror = function() {
            reject(new Error('Could not read file'));
        };
        reader.readAsDataURL(blob);
    });
}

function _stageAttachmentBlob(blob, name, mime, isImage, destinationHash, pending) {
    var stageToken = null;
    function checkCancelled() {
        if (pending && pending.cancelled) throw _attachmentCancelledError();
    }
    destinationHash = _canonicalConversationHash(destinationHash || lxmfActiveContact);
    return Promise.resolve().then(function() {
        checkCancelled();
        return RS.invoke('begin_attachment_stage', {
            args: {
                file_name: name,
                mime: mime || 'application/octet-stream',
                declared_size: blob.size,
                dest_hash: destinationHash || null,
                is_image: !!isImage,
            }
        });
    }).then(function(start) {
        stageToken = start.token;
        if (pending) pending.staging_token = stageToken;
        var chunkBytes = Math.min(Number(start.chunk_bytes) || (256 * 1024), 256 * 1024);
        var offset = 0;
        function appendNext() {
            checkCancelled();
            if (offset >= blob.size) return Promise.resolve(stageToken);
            var chunk = blob.slice(offset, Math.min(offset + chunkBytes, blob.size));
            return _readBlobBase64(chunk).then(function(base64) {
                checkCancelled();
                return RS.invoke('append_attachment_stage', {
                    args: {
                        token: stageToken,
                        offset: offset,
                        data_base64: base64,
                    }
                });
            }).then(function(result) {
                var written = Number(result && result.written);
                if (!Number.isSafeInteger(written) || written <= offset || written !== offset + chunk.size) {
                    throw new Error('Attachment staging returned an invalid offset');
                }
                offset = written;
                return appendNext();
            });
        }
        return appendNext();
    }).catch(function(error) {
        if (stageToken) {
            return RS.invoke('cancel_attachment_stage', { token: stageToken }).then(function() {
                if (pending && pending.staging_token === stageToken) pending.staging_token = null;
                throw error;
            });
        }
        throw error;
    });
}

function _looksLikeImageAttachment(file) {
    if (/^image\//i.test((file && file.type) || '')) return true;
    return /\.(?:avif|bmp|gif|heic|heif|jpe?g|png|webp)$/i.test((file && file.name) || '');
}

function _attachmentCancelledError() {
    var error = new Error('Attachment selection cancelled');
    error.cancelled = true;
    return error;
}

function _isCurrentPendingAttachment(pendingFile, token) {
    return token === _pendingAttachmentToken &&
        lxmfPendingFile === pendingFile &&
        pendingFile.destination === lxmfActiveContact &&
        !pendingFile.cancelled;
}

function _imagePreviewUrl(base64, mime) {
    if (!base64) return null;
    var raw = atob(base64);
    var bytes = new Uint8Array(raw.length);
    for (var i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
    return URL.createObjectURL(new Blob([bytes], { type: mime || 'image/jpeg' }));
}

function _imageProfileHint(profile) {
    if (profile === 'small') return 'Fastest to send';
    if (profile === 'medium') return 'Balanced quality and size';
    if (profile === 'large') return 'Higher quality';
    return 'Keep the most detail';
}

function _imageProfileLabel(profile) {
    if (profile === 'small') return 'Small';
    if (profile === 'medium') return 'Medium';
    if (profile === 'large') return 'Large';
    return 'Actual size';
}

function _chooseImageSize(file, inspection) {
    var dimensions = inspection.width && inspection.height
        ? inspection.width + ' × ' + inspection.height
        : 'Photo';
    var choices = (inspection.options || []).map(function(option) {
        var profile = option.profile;
        var estimate = Number(option.estimated_bytes) || Number(inspection.source_bytes) || file.size;
        return {
            label: option.label || _imageProfileLabel(profile),
            hint: _imageProfileHint(profile),
            meta: '~' + prettySize(estimate),
            value: profile,
            recommended: !!option.recommended,
            disabled: option.available === false,
        };
    });
    return rsChoice({
        title: 'Photo size',
        ariaLabel: 'Choose image attachment size',
        size: 'md',
        summary: {
            primary: _pendingAttachmentName(file),
            secondary: dimensions + ' · ' + prettySize(Number(inspection.source_bytes) || file.size),
            note: 'Smaller images send faster. Location and camera details are removed.',
        },
        choices: choices,
        cancelText: 'Cancel',
    });
}

function _chooseImageFileFallback(file, disposition) {
    var reason = disposition === 'animated'
        ? 'Animated images keep their motion when sent as files.'
        : (disposition === 'too_large'
            ? 'This image is too large to resize safely.'
            : 'This image format cannot be resized here.');
    return rsChoice({
        title: 'Send image as a file?',
        ariaLabel: 'Send image as an unchanged file',
        size: 'md',
        summary: {
            primary: _pendingAttachmentName(file),
            secondary: prettySize(file.size),
            note: reason + ' It will be sent unchanged, including its original metadata.',
        },
        choices: [{
            label: 'Send as file',
            hint: 'Keep the original file',
            meta: prettySize(file.size),
            value: 'file',
        }],
        cancelText: 'Cancel',
    });
}

function _finishPendingImageAsFile(pendingFile, stageToken) {
    return RS.invoke('mark_image_attachment_stage_as_file', {
        args: { token: stageToken }
    }).then(function() {
        pendingFile.inline_image = false;
        pendingFile.preparing = false;
        pendingFile.status_text = '';
        renderPendingFile();
        return stageToken;
    });
}

function _stageSelectedImage(file, pendingFile, selectionToken, nativeStage) {
    var stageToken = null;
    return (nativeStage || _stageAttachmentBlob(
        file,
        _pendingAttachmentName(file),
        file.type || 'application/octet-stream',
        true
    )).then(function(token) {
        stageToken = token;
        pendingFile.staging_token = token;
        if (!_isCurrentPendingAttachment(pendingFile, selectionToken)) {
            RS.invoke('cancel_attachment_stage', { token: token }).catch(function() {});
            throw _attachmentCancelledError();
        }
        pendingFile.status_text = 'Checking photo…';
        renderPendingFile();
        return RS.invoke('inspect_image_attachment_stage', { args: { token: token } });
    }).then(function(inspection) {
        if (!_isCurrentPendingAttachment(pendingFile, selectionToken)) throw _attachmentCancelledError();
        if (inspection.disposition === 'still' && !inspection.should_prompt) return { profile: 'actual', automatic: true };
        // Both in-app photos and system shares can arrive from a focused text
        // field. Release the IME before opening the non-editable size chooser.
        return RS.composer.dismissForReplacement(document.activeElement).then(function() {
            if (!_isCurrentPendingAttachment(pendingFile, selectionToken)) throw _attachmentCancelledError();
            if (inspection.disposition !== 'still') {
                pendingFile.status_text = 'Choose how to attach';
                renderPendingFile();
                return _chooseImageFileFallback(file, inspection.disposition).then(function(choice) {
                    if (choice !== 'file') throw _attachmentCancelledError();
                    return { as_file: true };
                });
            }
            pendingFile.status_text = 'Choose an image size';
            renderPendingFile();
            return _chooseImageSize(file, inspection).then(function(profile) {
                if (!profile) throw _attachmentCancelledError();
                return { profile: profile };
            });
        });
    }).then(function(choice) {
        if (!_isCurrentPendingAttachment(pendingFile, selectionToken)) throw _attachmentCancelledError();
        if (choice.as_file) return _finishPendingImageAsFile(pendingFile, stageToken);
        pendingFile.status_text = 'Preparing ' + _imageProfileLabel(choice.profile).toLowerCase() + '…';
        renderPendingFile();
        return RS.invoke('prepare_image_attachment_stage', {
            args: { token: stageToken, profile: choice.profile, automatic: !!choice.automatic }
        }).then(function(prepared) {
            if (!_isCurrentPendingAttachment(pendingFile, selectionToken)) {
                RS.invoke('cancel_attachment_stage', { token: stageToken }).catch(function() {});
                throw _attachmentCancelledError();
            }
            pendingFile.name = prepared.file_name;
            pendingFile.mime = prepared.mime;
            pendingFile.size = Number(prepared.size) || 0;
            pendingFile.inline_image = true;
            pendingFile.profile_label = _imageProfileLabel(prepared.profile);
            pendingFile.preview_url = _imagePreviewUrl(prepared.preview_base64, prepared.preview_mime);
            pendingFile.preparing = false;
            pendingFile.status_text = '';
            renderPendingFile();
            return stageToken;
        });
    }).catch(function(error) {
        if (error && error.cancelled) {
            if (stageToken) {
                RS.invoke('cancel_attachment_stage', { token: stageToken }).catch(function() {});
            }
            if (selectionToken === _pendingAttachmentToken && lxmfPendingFile === pendingFile) {
                clearPendingFile();
            }
            return null;
        }
        pendingFile.stage_error = error;
        pendingFile.preparing = false;
        if (stageToken) {
            RS.invoke('cancel_attachment_stage', { token: stageToken }).catch(function() {});
        }
        if (selectionToken === _pendingAttachmentToken && lxmfPendingFile === pendingFile) {
            showToast((error && error.message) || 'Could not prepare photo', 'toast-error', 4500);
            clearPendingFile();
        }
        return null;
    });
}

// System shares enter the exact same inspection/size-choice/preparation flow
// as a photo picked in the app. Only metadata and an opaque staging token cross
// IPC; source bytes never become a WebView File/base64/decoded image.
function attachSharedImage(image, nativeStage) {
    var ready = clearPendingFile();
    var token = ++_pendingAttachmentToken;
    var file = {name: image.name, size: image.size, type: image.mime};
    var pending = {name: image.name, size: image.size, mime: image.mime,
        inline_image: false, preparing: true, status_text: 'Staging photo…',
        preview_url: null, destination: lxmfActiveContact};
    lxmfPendingFile = pending;
    renderPendingFile();
    pending.source_stage_promise = Promise.resolve(ready).then(function() {
        if (pending.cancelled) throw _attachmentCancelledError();
        return typeof nativeStage === 'function' ? nativeStage() : nativeStage;
    });
    pending.stage_promise = _stageSelectedImage(file, pending, token, pending.source_stage_promise);
    return pending;
}

function handleFileSelected(inputEl) {
    var file = inputEl.files[0];
    if (!file) return;
    if (RS.rsnode && RS.rsnode.interceptFile(file, lxmfActiveContact)) {
        inputEl.value = '';
        return;
    }

    var maxSize = lxmfLimits.max_attachment_bytes || 128000000;
    if (file.size > maxSize) {
        showToast('File is too large for this network (' + prettySize(file.size) + ' > ' + prettySize(maxSize) + '). Choose a smaller file.', 'toast-warning', 5000);
        inputEl.value = '';
        clearPendingFile();
        return;
    }
    var imageCandidate = _looksLikeImageAttachment(file);
    if (!imageCandidate && file.size > (lxmfLimits.efficient_resource_bytes || 1048575)) {
        showToast('Large attachment — transfer may take a while on slow links', 'toast-info', 3500);
    }

    var ready = clearPendingFile();
    var token = ++_pendingAttachmentToken;
    var pendingFile = {
        name: _pendingAttachmentName(file),
        size: file.size,
        mime: file.type || 'application/octet-stream',
        inline_image: false,
        preparing: imageCandidate,
        status_text: imageCandidate ? 'Staging photo…' : '',
        preview_url: null,
        destination: lxmfActiveContact,
    };
    lxmfPendingFile = pendingFile;
    renderPendingFile();

    pendingFile.source_stage_promise = Promise.resolve(ready).then(function() {
        if (pendingFile.cancelled) throw _attachmentCancelledError();
        return _stageAttachmentBlob(file, pendingFile.name, pendingFile.mime,
            imageCandidate, pendingFile.destination, pendingFile);
    });
    if (imageCandidate) {
        pendingFile.stage_promise = _stageSelectedImage(file, pendingFile, token, pendingFile.source_stage_promise);
    } else {
        pendingFile.stage_promise = pendingFile.source_stage_promise.then(function(stageToken) {
            if (pendingFile.cancelled) {
                return RS.invoke('cancel_attachment_stage', { token: stageToken }).then(function() { return null; });
            }
            pendingFile.staging_token = stageToken;
            return stageToken;
        }).catch(function(error) {
            pendingFile.stage_error = error;
            if (!pendingFile.cancelled && lxmfPendingFile === pendingFile) {
                showToast((error && error.message) || 'Could not stage attachment', 'toast-error', 4500);
            }
            return null;
        });
    }
    inputEl.value = '';
}

// The pending attachment sits inside the composer pill, above the text.
function renderPendingFile() {
    var container = document.getElementById('lxmf-pending-file');
    if (!container) return;

    if (!lxmfPendingFile) {
        container.innerHTML = '';
        container.hidden = true;
        if (RS.composer.syncExtras) RS.composer.syncExtras(container);
        if (window.RS && RS.voiceMemos) RS.voiceMemos.syncComposer();
        return;
    }

    var isImage = lxmfPendingFile.inline_image === true;
    var meta = lxmfPendingFile.status_text
        ? lxmfPendingFile.status_text
        : ((lxmfPendingFile.profile_label ? lxmfPendingFile.profile_label + ' · ' : '') + prettySize(lxmfPendingFile.size));
    container.innerHTML = RS.composer.preview({
        thumb: isImage ? lxmfPendingFile.preview_url || '' : '',
        tile: isImage ? '' : 'file',
        text: lxmfPendingFile.name,
        meta: meta,
        closeLabel: 'Remove attachment'
    });
    if (lxmfPendingFile.preparing && !isImage) {
        var tile = container.querySelector('.ui-tile');
        if (tile) tile.innerHTML = '<span class="ui-spin" aria-hidden="true"></span>';
    }
    container.hidden = false;
    container.querySelector('[data-cmp-close]').addEventListener('click', clearPendingFile);
    RS.composer.syncExtras(container);
    if (window.RS && RS.voiceMemos) RS.voiceMemos.syncComposer();
}

function clearPendingFile() {
    _pendingAttachmentToken++;
    var pending = lxmfPendingFile;
    lxmfPendingFile = null;
    if (pending) {
        pending.cancelled = !pending.detached_for_send;
        if (!pending.detached_for_send) {
            // Await source staging, not a photo chooser that may still be open.
            // Cancelled Blob uploads stop at the next bounded read/append; a
            // late native token is retired before the next selection starts.
            var cleanup = Promise.resolve(pending.source_stage_promise).catch(function() {
                return null;
            }).then(function(token) {
                token = pending.staging_token || token;
                if (token) return RS.invoke('cancel_attachment_stage', { token: token });
            });
            _attachmentRetirement = Promise.all([_attachmentRetirement, cleanup]).then(function() {});
            // Event handlers may ignore the return value. The next selection
            // still observes cleanup failure and must not race a live lease.
            _attachmentRetirement.catch(function() {});
        }
        if (pending.preview_url) {
            try { URL.revokeObjectURL(pending.preview_url); } catch (_) {}
        }
    }
    var container = document.getElementById('lxmf-pending-file');
    if (container) {
        container.innerHTML = '';
        container.hidden = true;
        RS.composer.syncExtras(container);
    }
    if (window.RS && RS.voiceMemos) RS.voiceMemos.syncComposer();
    return _attachmentRetirement;
}

function setReplyTarget(msgData) {
    var replyContent = _messagePlainContent(msgData, !!msgData.audio).substring(0, 100);
    if (msgData.audio && !replyContent.trim()) {
        replyContent = (Number(msgData.audio.mode) === 0x10 || Number(msgData.audio.mode) === 0x03) ? 'Voice message' : 'Unsupported audio';
    }
    _replyTarget = {
        id: msgData.id,
        content: replyContent,
        sender: msgData.direction === 'inbound' ? msgData.source : msgData.destination,
        senderName: msgData.direction === 'inbound' ? _getContactName(msgData.source) : 'You',
    };
    var bar = document.getElementById('lxmf-reply-preview');
    if (bar) {
        var replyName = String(_replyTarget.senderName || '');
        if (msgData.direction === 'inbound' && !_conversationNameInfo(msgData.source, null, false).isHash) replyName = replyName.trim().split(/\s+/)[0];
        bar.querySelector('.reply-preview-sender').textContent = msgData.direction === 'inbound'
            ? 'Replying to ' + replyName : 'Replying to yourself';
        bar.querySelector('.reply-preview-text').textContent = _replyTarget.content;
        bar.hidden = false;
        RS.composer.syncExtras(bar);
    }
    var input = document.getElementById('lxmf-input');
    if (input) _focusLxmfComposerInput(input);
}

function clearReplyTarget() {
    _replyTarget = null;
    var bar = document.getElementById('lxmf-reply-preview');
    if (bar) {
        bar.hidden = true;
        RS.composer.syncExtras(bar);
    }
}

function _lookupAnnounceName(hash) {
    if (typeof PeersCache === 'undefined' || !PeersCache) return null;
    var entry = PeersCache.get(hash);
    return (entry && entry.display_name) ? entry.display_name.trim() : null;
}

function _getContactName(hash) {
    return _conversationNameInfo(hash, null, false).name;
}

function _messageSourceName(msg) {
    if (!msg) return 'Unknown';
    var info = _conversationNameInfo(msg.source, null, false);
    var emittedName = (typeof msg.source_display_name === 'string')
        ? msg.source_display_name.trim()
        : '';
    if (info.name && !info.isHash && info.name !== 'Anonymous') return info.name;
    if (emittedName) return emittedName;
    return info.name || emittedName || _hashFallbackName(msg.source);
}

var _activeContextMenu = null;
var _pendingMessageHoldActivation = null;
var _messageHoldActivationSequence = 0;
var _messagePointerContextSelection = null;

function _messageHoldActivationSurface(target, bubble) {
    if (!target || !target.closest || !bubble || !bubble.contains(target)) return null;
    return target.closest('.msg-img, .rs-message-link, .msg-file-save, .msg-quote, ' +
        '.md-spoiler:not(.is-revealed), .md-code-copy, .msg-mark--paper');
}

function _clearPendingMessageHoldActivation(expectedSequence) {
    if (!_pendingMessageHoldActivation) return false;
    if (expectedSequence && _pendingMessageHoldActivation.sequence !== expectedSequence) return false;
    if (_pendingMessageHoldActivation.expiryTimer) {
        clearTimeout(_pendingMessageHoldActivation.expiryTimer);
    }
    _pendingMessageHoldActivation = null;
    return true;
}

function _armPendingMessageHoldActivation(bubble, msgId, touch) {
    _clearPendingMessageHoldActivation();
    var touchTarget = touch && touch.target;
    if (!bubble || !msgId || !touchTarget || !bubble.contains(touchTarget)) return false;
    _pendingMessageHoldActivation = {
        sequence: ++_messageHoldActivationSequence,
        bubble: bubble,
        msgId: msgId,
        touchTarget: touchTarget,
        surface: _messageHoldActivationSurface(touchTarget, bubble),
        released: false,
        contextConsumed: false,
        activationConsumed: false,
        expiresAt: null,
        expiryTimer: null,
    };
    return true;
}

function _releasePendingMessageHoldActivation(bubble, now) {
    var pending = _pendingMessageHoldActivation;
    if (!pending || pending.bubble !== bubble || pending.released) return false;
    var timestamp = typeof now === 'number' ? now : Date.now();
    pending.released = true;
    pending.expiresAt = timestamp + 750;
    var sequence = pending.sequence;
    pending.expiryTimer = setTimeout(function() {
        _clearPendingMessageHoldActivation(sequence);
    }, 750);
    return true;
}

function _pendingMessageHoldContextMatches(target, bubble) {
    var pending = _pendingMessageHoldActivation;
    if (!pending || pending.bubble !== bubble || !target) return false;
    if (target === pending.touchTarget) return true;
    if (target.contains && target.contains(pending.touchTarget)) return true;
    return !!(pending.touchTarget.contains && pending.touchTarget.contains(target));
}

function _consumePendingMessageHoldContext(target, bubble, now) {
    var pending = _pendingMessageHoldActivation;
    var timestamp = typeof now === 'number' ? now : Date.now();
    if (!pending || (pending.expiresAt !== null && timestamp > pending.expiresAt)) {
        if (pending) _clearPendingMessageHoldActivation(pending.sequence);
        return false;
    }
    if (!_pendingMessageHoldContextMatches(target, bubble)) return false;
    pending.contextConsumed = true;
    return true;
}

function _consumePendingMessageHoldActivation(event, surface, now) {
    var pending = _pendingMessageHoldActivation;
    var timestamp = typeof now === 'number' ? now : Date.now();
    var bubble = surface && surface.closest ? surface.closest('.msg-bubble') : null;
    if (!pending || !pending.released || pending.activationConsumed || !surface ||
            pending.surface !== surface || bubble !== pending.bubble) {
        return false;
    }
    if (pending.expiresAt !== null && timestamp > pending.expiresAt) {
        _clearPendingMessageHoldActivation(pending.sequence);
        return false;
    }
    pending.activationConsumed = true;
    if (event && event.cancelable) event.preventDefault();
    if (event && typeof event.stopPropagation === 'function') event.stopPropagation();
    return true;
}

function _findRenderedMessageBubble(container, msgId) {
    if (!container || !msgId) return null;
    return Array.from(container.querySelectorAll('.msg-bubble[data-msg-id]')).find(function(candidate) {
        return candidate.getAttribute('data-msg-id') === msgId;
    }) || null;
}

function _messageActionShouldPreserveComposer() {
    return !!((_activeContextMenu && _activeContextMenu.preserveComposerKeyboard) || _shouldPreserveLxmfComposerKeyboard());
}

function _preventMessageActionFocusSteal(e) {
    if (_messageActionShouldPreserveComposer() && e && e.cancelable) e.preventDefault();
}

function _bindMessageFocusPreservingActivation(el, handler) {
    var touchHandled = false;
    function activate(e) {
        if (e) {
            if (e.cancelable) e.preventDefault();
            e.stopPropagation();
        }
        handler(e);
    }
    el.addEventListener('mousedown', _preventMessageActionFocusSteal);
    el.addEventListener('touchstart', _preventMessageActionFocusSteal, { passive: false });
    el.addEventListener('touchend', function(e) {
        if (!_messageActionShouldPreserveComposer()) return;
        var t = (e.changedTouches && e.changedTouches[0]) || null;
        if (t) {
            var hit = document.elementFromPoint(t.clientX, t.clientY);
            if (hit !== el && !el.contains(hit)) return;
        }
        touchHandled = true;
        setTimeout(function() { touchHandled = false; }, 500);
        activate(e);
    }, { passive: false });
    el.addEventListener('click', function(e) {
        if (touchHandled) {
            if (e.cancelable) e.preventDefault();
            e.stopPropagation();
            return;
        }
        activate(e);
    });
}

function _prefersKeyboardMessageFocus() {
    return !!(window.RS && RS.ui && typeof RS.ui.prefersKeyboardFocus === 'function' &&
        RS.ui.prefersKeyboardFocus());
}

function _messageActivationExpectsFocus(event) {
    if (_prefersKeyboardMessageFocus()) return true;
    if (!event) return false;
    if (event.type === 'click' && event.detail === 0) return true;
    return document.documentElement.dataset.inputModality === 'pointer';
}

function _focusMessageControl(control) {
    if (!control || typeof control.focus !== 'function') return;
    try { control.focus({ preventScroll: true }); }
    catch (_) { control.focus(); }
}

function _dismissContextMenu(opts) {
    opts = opts || {};
    if (!_activeContextMenu) return false;
    var state = _activeContextMenu;
    var trigger = state.trigger;
    var msgId = state.msgId;
    var restoreFocusExpected = state.restoreFocusExpected;
    if (state.menu && state.menu.parentNode) {
        state.menu.parentNode.removeChild(state.menu);
    }
    if (state.reposition) {
        window.removeEventListener('resize', state.reposition);
        if (state.container) state.container.removeEventListener('scroll', state.reposition);
        if (window.visualViewport) {
            window.visualViewport.removeEventListener('resize', state.reposition);
            window.visualViewport.removeEventListener('scroll', state.reposition);
        }
    }
    if (state.row) state.row.classList.remove('is-pressed');
    if (state.container) {
        state.container.classList.remove('is-acting');
        if (state.sheet) state.container.style.paddingBottom = '';
    }
    if (trigger && trigger.hasAttribute('aria-expanded')) trigger.setAttribute('aria-expanded', 'false');
    _activeContextMenu = null;
    // A choice collapses the hover actions; Escape or a click away leaves them as they were.
    if (state.hoverBar && opts.collapseHover) _closeHoverBar(state.hoverBar);
    var flushedRender = opts.flushDeferredRender !== false && _flushDeferredConversationRender();
    if (opts.restoreFocus !== false && restoreFocusExpected) {
        var focusTarget = trigger;
        if (flushedRender && trigger && msgId) {
            var container = document.getElementById('lxmf-messages');
            var bubble = _findRenderedMessageBubble(container, msgId);
            focusTarget = bubble;
            if (bubble && state.hoverAct && !opts.collapseHover) {
                var bar = bubble.querySelector('.msg-hover');
                if (bar) {
                    _openHoverBar(bar, false, false);
                    focusTarget = bar.querySelector('[data-hover-act="' + state.hoverAct + '"]') || bubble;
                }
            } else if (bubble && trigger.classList && trigger.classList.contains('msg-mark')) {
                focusTarget = bubble.querySelector('.msg-mark') || bubble;
            }
        }
        _focusMessageControl(focusTarget);
    }
    return true;
}

// Bulk selection. The tray's Select row selects a message; while anything is
// selected, taps on messages toggle them, a tap on empty transcript space or
// Cancel ends it, and the bar under the header offers "Delete (N)".
var _msgSelection = null;

function _conversationMessageById(id) {
    for (var i = 0; i < lxmfConversation.length; i++) {
        if (lxmfConversation[i].id === id) return lxmfConversation[i];
    }
    return null;
}

function _messageSelectable(msg) {
    // A message still sending or waiting would be delivered anyway; stop it first.
    return !!(msg && msg.id) && !_messageCanCancelSend(msg) && msg.state !== 'waiting';
}

function _messageSelectionActive() {
    return !!(_msgSelection && _msgSelection.ids.length &&
        _msgSelection.ownerHash === _canonicalConversationHash(lxmfActiveContact));
}

function _selectMessage(msg) {
    if (!_messageSelectable(msg)) return;
    var owner = _canonicalConversationHash(lxmfActiveContact);
    if (!_msgSelection || _msgSelection.ownerHash !== owner) {
        _msgSelection = { ownerHash: owner, ids: [] };
    }
    if (_msgSelection.ids.indexOf(msg.id) === -1) _msgSelection.ids.push(msg.id);
    _applyMessageSelection();
}

function _toggleMessageSelection(msg) {
    if (!_messageSelectionActive() || !_messageSelectable(msg)) return;
    var index = _msgSelection.ids.indexOf(msg.id);
    if (index === -1) _msgSelection.ids.push(msg.id);
    else _msgSelection.ids.splice(index, 1);
    if (!_msgSelection.ids.length) _msgSelection = null;
    _applyMessageSelection();
    if (typeof haptic === 'function') haptic('selection');
}

function _clearMessageSelection() {
    if (!_msgSelection) return false;
    var wasActive = _messageSelectionActive();
    _msgSelection = null;
    _applyMessageSelection();
    return wasActive;
}

function _messageSelectLabel(msg) {
    var when = msg.timestamp ? new Date(msg.timestamp * 1000).toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' }) : '';
    var text = String(_messagePlainContent(msg, !!msg.audio) || '').trim().slice(0, 60) ||
        (msg.image ? 'Photo' : msg.audio ? 'Voice message' : 'File');
    return (msg.direction === 'outbound' ? 'You' : 'Them') + (when ? ', ' + when : '') + ': ' + text;
}

var _msgSelectAnnounceTimer = null;

function _applyMessageSelection() {
    var active = _messageSelectionActive();
    if (active) {
        _msgSelection.ids = _msgSelection.ids.filter(function(id) { return !!_conversationMessageById(id); });
        if (!_msgSelection.ids.length) {
            _msgSelection = null;
            active = false;
        }
    }
    var ids = active ? _msgSelection.ids : [];
    var container = document.getElementById('lxmf-messages');
    if (container) {
        container.classList.toggle('is-selecting', active);
        container.querySelectorAll('.msg-row').forEach(function(row) {
            var bubble = row.querySelector('.msg-bubble[data-msg-id]');
            var check = row.querySelector('.msg-check');
            var msg = bubble ? _conversationMessageById(bubble.getAttribute('data-msg-id')) : null;
            // In-flight sends get no box at all: they cannot be selected.
            if (!active || !msg || !_messageSelectable(msg)) {
                if (check) check.remove();
                row.classList.remove('is-selected');
                row.removeAttribute('aria-selected');
                return;
            }
            var selected = ids.indexOf(msg.id) !== -1;
            if (!check) {
                row.insertAdjacentHTML('afterbegin', '<label class="ui-check-row msg-check"><input type="checkbox">' +
                    '<span class="ui-check">' + RS.kit.icon('check') + '</span></label>');
                check = row.firstChild;
                check.querySelector('input').setAttribute('aria-label', _messageSelectLabel(msg));
            }
            check.querySelector('input').checked = selected;
            row.classList.toggle('is-selected', selected);
            if (selected) row.setAttribute('aria-selected', 'true');
            else row.removeAttribute('aria-selected');
        });
    }
    _applyMessageSelectBar(active, ids.length, container);
}

// While selecting, the header becomes Cancel, "2 selected", and Delete; the composer steps away.
function _applyMessageSelectBar(active, count, container) {
    var bar = document.getElementById('lxmf-select-bar');
    if (!bar) return;
    var wasShown = !bar.hidden;
    if (active !== wasShown) {
        bar.hidden = !active;
        var header = document.getElementById('lxmf-chat-header');
        if (header) header.hidden = active;
        var threads = document.getElementById('lxmf-chat-top');
        if (threads) threads.classList.toggle('is-selecting', active);
        var composer = document.getElementById('lxmf-composer');
        if (composer) composer.classList.toggle('is-selecting', active);
        if (active) {
            var input = document.getElementById('lxmf-input');
            if (input && document.activeElement === input) input.blur();
        } else if (bar.contains(document.activeElement)) {
            var composerInput = document.getElementById('lxmf-input');
            if (composerInput && !isCompactLayout()) _focusMessageControl(composerInput);
        }
    }
    var deleteBtn = document.getElementById('lxmf-select-delete-btn');
    if (deleteBtn) {
        deleteBtn.setAttribute('aria-label', 'Delete ' + count + (count === 1 ? ' selected message' : ' selected messages'));
    }
    var status = document.getElementById('lxmf-select-status');
    if (status && (active || wasShown)) {
        status.textContent = active ? count + ' selected' : '';
    }
}

// Clicks that leave the conversation end selection: the conversation list or app navigation.
// Cancel, Esc, and Back do too.
var MSG_SELECTION_EXIT_SURFACES = '.lxmf-sidebar, #sidebar, #bottom-bar';

function _handleMessageSelectionClick(e) {
    if (!_messageSelectionActive()) return;
    var target = e.target;
    var container = document.getElementById('lxmf-messages');
    if (!container || !target || !container.contains(target)) {
        if (!_msgSelection.pending && target && target.closest &&
                target.closest(MSG_SELECTION_EXIT_SURFACES)) {
            _clearMessageSelection();
        }
        return;
    }
    // Anywhere on a message's row toggles it, including its box and the space beside
    // the bubble; the gaps between messages do nothing.
    var row = target.closest ? target.closest('.msg-row') : null;
    if (!row) return;
    // The click synthesized by the hold that selected this message is not a toggle.
    if (_consumePendingMessageHoldActivation(e, e.target)) return;
    e.preventDefault();
    e.stopPropagation();
    var bubble = row.querySelector('.msg-bubble[data-msg-id]');
    var msg = bubble ? _conversationMessageById(bubble.getAttribute('data-msg-id')) : null;
    _dismissContextMenu({ restoreFocus: false });
    if (msg) _toggleMessageSelection(msg);
}

function _removeConversationMessages(hash, ids) {
    var owner = _canonicalConversationHash(hash);
    var key = _conversationKey(owner, _resolveConversationThread(owner));
    var drop = function(list) {
        return (list || []).filter(function(m) { return ids.indexOf(m.id) === -1; });
    };
    var cached = cacheGet(key);
    if (cached) cacheSet(key, drop(cached));
    ids.forEach(function(id) { delete _msgReactions[id]; });
    if (key === _activeConversationKey()) {
        lxmfConversation = drop(lxmfConversation);
        if (_msgSelection) {
            _msgSelection.ids = _msgSelection.ids.filter(function(id) { return ids.indexOf(id) === -1; });
        }
        renderConversation();
    }
}

function _deleteSelectedMessages() {
    if (!_messageSelectionActive() || _msgSelection.pending) return;
    var selection = _msgSelection;
    var ids = selection.ids.slice();
    var count = ids.length;
    var hash = lxmfActiveContact;
    var nameInfo = _conversationNameInfo(hash, null, false);
    var who = nameInfo && !nameInfo.isHash && nameInfo.name ? nameInfo.name : 'The other person';
    var hasMedia = ids.some(function(id) {
        var m = _conversationMessageById(id);
        return !!(m && (m.image || m.audio || (Array.isArray(m.attachments) && m.attachments.length)));
    });
    var message = (count === 1 ? 'It will' : 'They will') + ' be removed from this device only. ' +
        who + ' still has their copy. This can’t be undone.' +
        (hasMedia ? '\n\nPhotos and files in ' + (count === 1 ? 'this message' : 'these messages') +
            ' will be deleted from this device too.' : '');
    selection.pending = true;
    if (typeof haptic === 'function') haptic('warning');
    rsConfirm({
        title: count === 1 ? 'Delete message?' : 'Delete ' + count + ' messages?',
        message: message,
        confirmText: count > 1 ? 'Delete ' + count : 'Delete',
        cancelText: 'Cancel',
        danger: true,
    }).then(function(confirmed) {
        if (!confirmed || _msgSelection !== selection) {
            selection.pending = false;
            return;
        }
        return RS.invoke('delete_messages', { hash: hash, ids: ids }).then(function() {
            if (_msgSelection === selection) _msgSelection = null;
            _removeConversationMessages(hash, ids);
            _applyMessageSelection();
            if (typeof showToast === 'function') {
                showToast(count + (count === 1 ? ' message deleted.' : ' messages deleted.'), 'toast-success', 2000);
            }
            if (typeof haptic === 'function') haptic('success');
        }).catch(function(err) {
            if (err && err.code === 'files_not_removed') {
                // The rows are gone; only some files are left for the cleanup sweep.
                if (_msgSelection === selection) _msgSelection = null;
                _removeConversationMessages(hash, ids);
                _applyMessageSelection();
                if (typeof showToast === 'function') {
                    showToast('Messages deleted. Some files could not be removed.', 'toast-warning', 3500);
                }
                return;
            }
            selection.pending = false;
            if (typeof showToast === 'function') {
                showToast('Could not delete messages: ' + ((err && err.message) || 'unknown error'), 'toast-error', 3500);
            }
            if (typeof haptic === 'function') haptic('warning');
        });
    });
}

function _messageActionOwnsText(bubble, target) {
    if (!_activeContextMenu || !bubble || _activeContextMenu.bubble !== bubble) return false;
    if (_activeContextMenu.ownerHash !== _canonicalConversationHash(lxmfActiveContact)) return false;
    var content = target && target.closest ? target.closest('.msg-text') : null;
    return !!(content && bubble.contains(content));
}

function _messageElevatedTextStartsNativeSelection(touch, bubble) {
    if (!_messageActionOwnsText(bubble, touch && touch.target)) return false;
    // The first hold's synthetic context/click guard must never consume the
    // new gesture that intentionally begins native selection.
    _clearPendingMessageHoldActivation();
    return true;
}

function _clearNativeMessageSelection() {
    if (!window.getSelection) return;
    var selection = window.getSelection();
    if (selection && typeof selection.removeAllRanges === 'function') selection.removeAllRanges();
}

function _messageSelectionIntersectsBubble(bubble) {
    if (!bubble || !window.getSelection) return false;
    var selection = window.getSelection();
    if (!selection || selection.isCollapsed || selection.rangeCount < 1) return false;
    var range = selection.getRangeAt(0);
    if (range && typeof range.intersectsNode === 'function') {
        try { return range.intersectsNode(bubble); }
        catch (_) {}
    }
    var ancestor = range && range.commonAncestorContainer;
    return !!ancestor && bubble.contains(ancestor);
}

function _messageTouchStartsDirectControl(touch, bubble) {
    var target = touch && touch.target;
    if (!target || !target.closest || !bubble || !bubble.contains(target)) return false;
    return !!target.closest(
        '.msg-hover, .msg-cancel, .msg-mark, .msg-react, ' +
        '.voice-memo-player-play, .voice-memo-player-waveform, .voice-memo-player-download, .md-code-copy'
    );
}

// The desktop link menu offers "Open Link", which would skip the leaving-app
// warning; pointer right-clicks open the message actions (with Copy Link).
function _messageLinkUrlAt(target) {
    var link = target && target.closest ? target.closest('.rs-message-link[data-url]') : null;
    return link ? link.getAttribute('data-url') : null;
}

function _openExternalMessageLink(url) {
    if (!(window.RS && typeof RS.openExternalUrl === 'function')) return;
    RS.openExternalUrl(url).catch(function(err) {
        showToast('Could not open link: ' + ((err && err.message) || 'error'), 'toast-error', 3500);
    });
}

// Hosts the user chose not to be asked about again (device-wide setting).
RS.linkTrust = (function() {
    var hosts = [];
    var loading = null;
    function apply(resp) {
        hosts = resp && Array.isArray(resp.hosts) ? resp.hosts.slice() : hosts;
        if (typeof updateTrustedLinksButton === 'function') updateTrustedLinksButton(hosts.slice());
        return hosts.slice();
    }
    return {
        load: function() {
            if (!loading) {
                loading = RS.invoke('api_trusted_link_hosts').then(apply).catch(function() {
                    loading = null;
                    return hosts.slice();
                });
            }
            return loading;
        },
        list: function() { return hosts.slice(); },
        has: function(host) { return hosts.indexOf(host) >= 0; },
        add: function(host) { return RS.invoke('trust_link_host', { host: host }).then(apply); },
        remove: function(host) { return RS.invoke('untrust_link_host', { host: host }).then(apply); },
        clear: function() { return RS.invoke('clear_trusted_link_hosts').then(apply); },
    };
})();

function _confirmExternalLink(url, link) {
    // Paper links open in Ratspeak, so the leaving-app warning doesn't apply.
    if (/^lxm:\/\//i.test(String(url || ''))) {
        if (typeof openPaperDecoder === 'function') openPaperDecoder({ uri: url, autoReview: true });
        return;
    }
    var masked = link && link.hasAttribute('data-masked');
    var linkText = masked ? link.textContent : null;
    var target = rsLinkTarget(url, linkText);
    if (!target) return;
    RS.linkTrust.load().then(function() {
        if (!target.warnings.length && RS.linkTrust.has(target.trustKey)) {
            _openExternalMessageLink(target.href);
            return;
        }
        rsLinkConfirm(url, { linkText: linkText }).then(function(result) {
            if (!result.confirmed) return;
            if (result.trust) {
                RS.linkTrust.add(result.host).catch(function() {
                    showToast('Could not save website', 'toast-error', 2500);
                });
            }
            _openExternalMessageLink(target.href);
        });
    });
}

// Only code blocks that actually overflow become keyboard-scrollable stops.
function _syncCodeBlockFocus(pre) {
    if (pre.scrollWidth > pre.clientWidth) {
        var lang = pre.getAttribute('data-lang');
        pre.setAttribute('tabindex', '0');
        pre.setAttribute('aria-label', lang ? 'Code, ' + lang : 'Code');
    } else {
        pre.removeAttribute('tabindex');
        pre.removeAttribute('aria-label');
    }
}

var _codeBlockObserver = typeof ResizeObserver === 'function'
    ? new ResizeObserver(function(entries) {
        entries.forEach(function(entry) { _syncCodeBlockFocus(entry.target); });
    })
    : null;

function _bindMessageCodeBlocks(container) {
    if (_codeBlockObserver) _codeBlockObserver.disconnect();
    container.querySelectorAll('.md-codeblock').forEach(function(block) {
        var pre = block.querySelector('.md-pre');
        if (pre) {
            _syncCodeBlockFocus(pre);
            if (_codeBlockObserver) _codeBlockObserver.observe(pre);
        }
        var copy = block.querySelector('.md-code-copy');
        if (!copy) return;
        copy.addEventListener('click', function(e) {
            e.preventDefault();
            e.stopPropagation();
            if (_consumePendingMessageHoldActivation(e, copy) || _messageSelectionActive()) return;
            if (copy.closest('.md-spoiler:not(.is-revealed)')) return;
            var code = pre ? pre.textContent : '';
            Promise.resolve(RS.copyText(code)).then(function() {
                showToast('Code copied', 'toast-success', 1800);
                if (typeof haptic === 'function') haptic('light');
                copy.classList.add('is-copied');
                clearTimeout(copy._copiedTimer);
                copy._copiedTimer = setTimeout(function() { copy.classList.remove('is-copied'); }, 1200);
            }).catch(function() {
                showToast('Could not copy', 'toast-error', 1800);
            });
        });
    });
}

// Revealed spoilers stay revealed across re-renders until the chat changes.
var _revealedSpoilers = { hash: null, keys: {} };

function _spoilerKey(spoiler) {
    var bubble = spoiler.closest('.msg-bubble[data-msg-id]');
    if (!bubble) return null;
    var all = bubble.querySelectorAll('.md-spoiler');
    return bubble.getAttribute('data-msg-id') + ':' + Array.prototype.indexOf.call(all, spoiler);
}

function _revealMessageSpoiler(spoiler, restoring) {
    var hadFocus = !restoring && document.activeElement === spoiler;
    spoiler.classList.add('is-revealed');
    spoiler.removeAttribute('role');
    spoiler.removeAttribute('aria-label');
    spoiler.removeAttribute('aria-expanded');
    var content = spoiler.querySelector('.md-spoiler-content');
    if (content) {
        content.removeAttribute('aria-hidden');
        content.removeAttribute('inert');
    }
    if (hadFocus) {
        spoiler.setAttribute('tabindex', '-1');
        spoiler.focus({ preventScroll: true });
    } else {
        spoiler.removeAttribute('tabindex');
    }
    if (restoring) return;
    var key = _spoilerKey(spoiler);
    if (key) _revealedSpoilers.keys[key] = true;
}

function _bindMessageSpoilers(container) {
    var hash = _canonicalConversationHash(lxmfActiveContact);
    if (_revealedSpoilers.hash !== hash) _revealedSpoilers = { hash: hash, keys: {} };
    container.querySelectorAll('.md-spoiler').forEach(function(spoiler) {
        if (_revealedSpoilers.keys[_spoilerKey(spoiler)]) {
            _revealMessageSpoiler(spoiler, true);
            return;
        }
        spoiler.addEventListener('click', function(e) {
            if (spoiler.classList.contains('is-revealed') || _messageSelectionActive()) return;
            e.preventDefault();
            e.stopPropagation();
            if (_consumePendingMessageHoldActivation(e, spoiler)) return;
            _revealMessageSpoiler(spoiler);
        });
        spoiler.addEventListener('keydown', function(e) {
            if (spoiler.classList.contains('is-revealed') || (e.key !== 'Enter' && e.key !== ' ')) return;
            e.preventDefault();
            e.stopPropagation();
            _revealMessageSpoiler(spoiler);
        });
    });
}

function _messageLinkActivationAllowed(link) {
    var bubble = link && link.closest ? link.closest('.msg-bubble') : null;
    return !_messageActionOwnsText(bubble, link) && !_messageSelectionIntersectsBubble(bubble);
}

function _rememberMessagePointerContextSelection(event, bubble) {
    if (!event || event.button !== 2 || !bubble) return false;
    _messagePointerContextSelection = {
        bubble: bubble,
        hadSelection: _messageSelectionIntersectsBubble(bubble),
    };
    return true;
}

function _consumeMessagePointerContextSelection(bubble) {
    var record = _messagePointerContextSelection;
    _messagePointerContextSelection = null;
    if (!record || record.bubble !== bubble) return null;
    return record.hadSelection;
}

function _messageContextMenuDisposition(target, bubble, now, selectionExistedBeforePointer) {
    // A contextmenu synthesized from the first action-opening hold belongs to
    // that exact gesture. Suppress it before considering the newly elevated
    // bubble; a real second hold has already cleared this pending record.
    if (_consumePendingMessageHoldContext(target, bubble, now)) return 'suppress';
    if (_messageLinkUrlAt(target) && document.documentElement.dataset.inputModality !== 'touch') return 'actions';
    var selectableText = target && target.closest && target.closest('.msg-text');
    var preserveSelection = selectionExistedBeforePointer === null ||
        typeof selectionExistedBeforePointer === 'undefined'
        ? _messageSelectionIntersectsBubble(bubble)
        : selectionExistedBeforePointer;
    if ((selectableText && _messageActionOwnsText(bubble, target)) || preserveSelection) {
        return 'native';
    }
    return 'actions';
}

function _ownReactionSender() {
    if (lxmfIdentity) {
        if (lxmfIdentity.lxmf_hash) return lxmfIdentity.lxmf_hash;
        if (lxmfIdentity.lxmf_destination) return lxmfIdentity.lxmf_destination;
        if (lxmfIdentity.hash && lxmfIdentity.hash !== lxmfIdentity.identity_hash) return lxmfIdentity.hash;
    }
    if (typeof identityList !== 'undefined' && Array.isArray(identityList)) {
        for (var i = 0; i < identityList.length; i++) {
            if (identityList[i].is_active && identityList[i].lxmf_hash) return identityList[i].lxmf_hash;
        }
    }
    return '';
}

function _hasOwnReaction(msgId, emoji) {
    var sender = _ownReactionSender();
    if (!sender) return false;
    var reactions = _msgReactions[msgId] || [];
    return reactions.some(function(r) { return r.sender === sender && r.emoji === emoji; });
}

function _optimisticApplyReaction(msgId, emoji, action) {
    var sender = _ownReactionSender();
    if (!sender || !msgId || !emoji) return;
    var reactions = (_msgReactions[msgId] || []).slice();
    reactions = reactions.filter(function(r) {
        return !(r.sender === sender && r.emoji === emoji);
    });
    if (action !== 'remove') {
        reactions.push({ sender: sender, emoji: emoji, timestamp: Date.now() / 1000 });
    }
    _msgReactions[msgId] = reactions;
    for (var i = 0; i < lxmfConversation.length; i++) {
        if (lxmfConversation[i].id === msgId) {
            lxmfConversation[i].reactions = reactions;
            break;
        }
    }
    renderConversation();
}

function _restoreRenderedMessageActionFocus(msgId, emoji, focusExpected) {
    if (!(focusExpected || _prefersKeyboardMessageFocus()) || !msgId) return;
    var container = document.getElementById('lxmf-messages');
    if (!container) return;
    var bubble = Array.from(container.querySelectorAll('.msg-bubble[data-msg-id]')).find(function(candidate) {
        return candidate.getAttribute('data-msg-id') === msgId;
    });
    var target = null;
    if (emoji) {
        target = Array.from(container.querySelectorAll('.msg-react[data-msg-id]')).find(function(candidate) {
            return candidate.getAttribute('data-msg-id') === msgId &&
                candidate.getAttribute('data-emoji') === emoji;
        }) || null;
    }
    if (!target) target = bubble;
    _focusMessageControl(target);
}

function _sendReactionForMessage(msgData, emoji, opts) {
    opts = opts || {};
    if (!msgData || !msgData.id || !emoji) return;
    var shouldRestoreComposer = _messageActionShouldPreserveComposer();
    var focusExpected = !!(opts.focusExpected ||
        (_activeContextMenu && _activeContextMenu.restoreFocusExpected));
    var action = _hasOwnReaction(msgData.id, emoji) ? 'remove' : 'add';
    if (opts.dismiss !== false) {
        // The optimistic reaction render consumes any coalesced transcript
        // update, so keep this as one rebuild after the action has activated.
        _dismissContextMenu({ flushDeferredRender: false });
    }
    _optimisticApplyReaction(msgData.id, emoji, action);
    _flushDeferredConversationRender();
    _restoreRenderedMessageActionFocus(
        msgData.id,
        opts.restoreReactionFocus ? emoji : null,
        focusExpected
    );
    _restoreLxmfComposerKeyboard(shouldRestoreComposer);
    if (typeof haptic === 'function') haptic('selection');
    RS.invoke('send_reaction', {
        args: {
            dest_hash: lxmfActiveContact,
            message_id: msgData.id,
            emoji: emoji,
            action: action,
            delivery_method: 'auto'
        }
    }).catch(function() {
        // Roll the optimistic apply back so the pill reflects reality.
        _optimisticApplyReaction(msgData.id, emoji, action === 'add' ? 'remove' : 'add');
        _restoreRenderedMessageActionFocus(
            msgData.id,
            opts.restoreReactionFocus ? emoji : null,
            focusExpected
        );
        if (typeof showToast === 'function') showToast('Could not add the reaction', 'toast-error', 2500);
    });
}

function _copyToClipboard(text) {
    if (!text) return Promise.resolve(false);
    return RS.copyText(text);
}

function _resolveMessageImageFile(msgData) {
    if (!msgData || !msgData.image) return Promise.reject(new Error('No image selected'));
    var image = msgData.image || {};
    var filename = image.filename || 'image';
    var mime = image.mime_type || image.mime || '';
    if (image.stored_name) {
        var cached = _getImageDownloadFile(image.stored_name);
        if (cached) return Promise.resolve(cached);
        return RS.fileDownload(image.stored_name).then(function(file) {
            _rememberImageBlobUrl(image.stored_name, file.url, file);
            return file;
        });
    }
    if (image.data_url) {
        var file = _fileFromDataUrl(image.data_url, filename, mime);
        if (file) return Promise.resolve(file);
    }
    return Promise.reject(new Error('Image data is not ready yet'));
}

function _messageFirstAttachment(msgData) {
    if (!msgData || !Array.isArray(msgData.attachments) || msgData.attachments.length === 0) return null;
    for (var i = 0; i < msgData.attachments.length; i++) {
        if (msgData.attachments[i] && msgData.attachments[i].stored_name) return msgData.attachments[i];
    }
    return msgData.attachments[0] || null;
}

function _resolveMessageAttachmentFile(att) {
    if (!att || !att.stored_name) return Promise.reject(new Error('File is not ready yet'));
    return RS.fileDownload(att.stored_name);
}

function _saveDownloadedMediaFile(file, opts) {
    opts = opts || {};
    return RS.saveDownloadedFile(file, opts).then(function() {
        if (typeof showToast === 'function') {
            showToast(/^image\//i.test(file.mime || '') && opts.preferPhotos ? 'Saved to Photos' : 'Saved', 'toast-success', 2500);
        }
        return true;
    });
}

function _messageMediaContextAction(msgData) {
    if (msgData && msgData.image) {
        var image = msgData.image;
        var canCopyImage = _canCopyDownloadedImages();
        return {
            label: canCopyImage ? 'Copy image' : 'Save image',
            icon: canCopyImage ? 'copy' : 'save',
            run: function() {
                if (!canCopyImage && image.stored_name && typeof isTauriMobile === 'function' && isTauriMobile()) {
                    return RS.saveFile(image.stored_name, { preferPhotos: true });
                }
                return _resolveMessageImageFile(msgData).then(function(file) {
                    if (canCopyImage) {
                        return _copyDownloadedImage(file).then(function() {
                            if (typeof showCopyConfirmationToast === 'function') showCopyConfirmationToast('Image');
                            return true;
                        });
                    }
                    return _saveDownloadedMediaFile(file, { preferPhotos: true });
                });
            }
        };
    }
    var attachment = _messageFirstAttachment(msgData);
    if (attachment) {
        return {
            label: 'Save file',
            icon: 'save',
            run: function() {
                if (attachment.stored_name &&
                    ((typeof isTauriMobile === 'function' && isTauriMobile()) ||
                     (typeof isTauriDesktop === 'function' && isTauriDesktop()))) {
                    return RS.saveFile(attachment.stored_name);
                }
                return _resolveMessageAttachmentFile(attachment).then(function(file) {
                    return _saveDownloadedMediaFile(file);
                });
            }
        };
    }
    return null;
}

var MSG_ACTION_GAP = 8;
var MSG_ACTION_EDGE = 12;
var MSG_QUICK_REACTIONS = ['❤️', '👍', '😂', '😮', '😢', '🔥'];

// Visible transcript band in layout-viewport coordinates: below the header
// (and call strip), above the composer and the on-screen keyboard.
function _messageActionBand() {
    var vv = window.visualViewport;
    var top = vv ? vv.offsetTop : 0;
    var bottom = vv ? vv.offsetTop + vv.height : window.innerHeight;
    ['lxmf-chat-top', 'lxst-call-strip'].forEach(function(id) {
        var el = document.getElementById(id);
        if (!el || el.hidden) return;
        var r = el.getBoundingClientRect();
        if (r.height > 0) top = Math.max(top, r.bottom);
    });
    var composer = document.getElementById('lxmf-composer');
    if (composer && !composer.hidden) {
        var cr = composer.getBoundingClientRect();
        if (cr.height > 0) bottom = Math.min(bottom, cr.top);
    }
    return { top: top + MSG_ACTION_GAP, bottom: bottom - MSG_ACTION_GAP };
}

// Smallest transcript shift (+ scrolls down) that brings a message of the
// given top and bottom between the band's top and `limit`, preferring its top.
function _messageActionScrollDelta(top, bottom, band, limit) {
    var lower = Math.min(band.bottom, limit);
    if (bottom - top > lower - band.top) return top - band.top;
    if (bottom > lower) return bottom - lower;
    if (top < band.top) return top - band.top;
    return 0;
}

// Arrow-key target within the menu. Groups are in on-screen order; the
// reactions row moves with Left/Right (wrapping), the action rows with
// Up/Down, and Up/Down at an edge crosses to the neighbouring group.
function _messageActionKeyTarget(groups, current, key) {
    var gi = -1;
    var ii = -1;
    groups.forEach(function(group, i) {
        var j = group.items.indexOf(current);
        if (j !== -1) { gi = i; ii = j; }
    });
    if (gi === -1) return null;
    var items = groups[gi].items;
    var horizontal = groups[gi].horizontal;
    if (key === 'Home') return items[0];
    if (key === 'End') return items[items.length - 1];
    var enter = function(step) {
        var next = groups[gi + step];
        if (!next || !next.items.length) return null;
        return next.horizontal || step > 0 ? next.items[0] : next.items[next.items.length - 1];
    };
    if (horizontal) {
        if (key === 'ArrowLeft' || key === 'ArrowRight') {
            return items[(ii + (key === 'ArrowRight' ? 1 : -1) + items.length) % items.length];
        }
        if (key === 'ArrowUp' || key === 'ArrowDown') return enter(key === 'ArrowDown' ? 1 : -1);
        return null;
    }
    if (key !== 'ArrowUp' && key !== 'ArrowDown') return null;
    var step = key === 'ArrowDown' ? 1 : -1;
    return items[ii + step] || enter(step);
}

// Desktop: one kit menu under its anchor (above it when there is no room), docked inside
// the transcript band when neither fits. Phone: a sheet, with the pressed message kept in view.
function _layoutMessageActionPanels(state, allowScroll) {
    var bubble = state.bubble;
    if (!bubble || !bubble.isConnected) return null;
    var menu = state.menu;
    var band = _messageActionBand();
    var b = bubble.getBoundingClientRect();
    var rowRect = state.row && state.row.isConnected ? state.row.getBoundingClientRect() : b;
    var top = b.top;
    var bottom = Math.max(b.bottom, rowRect.bottom);

    if (state.sheet) {
        var sheetTop = menu.getBoundingClientRect().top - MSG_ACTION_GAP;
        var scroller = document.getElementById('lxmf-messages');
        var delta = _messageActionScrollDelta(top, bottom, band, sheetTop);
        if (allowScroll && scroller && delta) {
            var before = scroller.scrollTop;
            // Room under the last message so the newest can rise above the sheet too.
            var room = Math.max(0, bottom - sheetTop);
            if (room) scroller.style.paddingBottom = (room + MSG_ACTION_GAP) + 'px';
            scroller.scrollTop = before + delta;
        }
        menu.setAttribute('data-placement', 'sheet');
        return 'sheet';
    }

    var a = state.anchorRect || b;
    var h = menu.offsetHeight;
    var w = menu.offsetWidth;
    var g = 4;
    var placement;
    var menuTop;
    // A message partly under the header (a keyboard opened it) keeps its menu inside the band.
    if (a.bottom + g + h <= band.bottom) {
        placement = 'below';
        menuTop = Math.max(band.top, a.bottom + g);
    } else if (a.top - g - h >= band.top) {
        placement = 'above';
        menuTop = a.top - g - h;
    } else {
        // Taller than the room around it: dock over the transcript, never over the chrome.
        placement = 'dock';
        menuTop = Math.max(band.top, Math.min(a.top, band.bottom - h));
    }
    var insets = safeAreaInsets();
    var left = state.outbound ? a.right - w : a.left;
    left = Math.max(MSG_ACTION_EDGE + insets.left,
        Math.min(left, window.innerWidth - w - MSG_ACTION_EDGE - insets.right));
    menu.style.left = Math.round(left) + 'px';
    menu.style.top = Math.round(menuTop) + 'px';
    menu.setAttribute('data-placement', placement);
    return placement;
}

function _prepareMessageActionTarget(msgData, bubble, trigger, x, y) {
    return { bubble: bubble, trigger: trigger, x: x, y: y };
}

function _messageMenuButton(cls, action, glyph, label) {
    var btn = document.createElement('button');
    btn.type = 'button';
    btn.className = cls;
    btn.setAttribute('data-message-action', action);
    btn.innerHTML = RS.kit.icon(glyph) + '<span class="lbl">' + escapeHtml(label) + '</span>';
    return btn;
}

// The message menu: a reactions row on top, then the actions. opts: { focusDialog, linkUrl,
// sheet (a long press), anchor (element), reactOnly (the hover bar's React) }.
function _showMsgContextMenu(msgData, x, y, bubble, trigger, opts) {
    opts = opts || {};
    var preparedTarget = _prepareMessageActionTarget(msgData, bubble, trigger, x, y);
    if (!preparedTarget) return;
    bubble = preparedTarget.bubble;
    trigger = preparedTarget.trigger;
    x = preparedTarget.x;
    y = preparedTarget.y;
    _dismissContextMenu({ restoreFocus: false, flushDeferredRender: false });

    var preserveComposerKeyboard = _shouldPreserveLxmfComposerKeyboard();
    if (typeof haptic === 'function') haptic('selection');
    var container = document.getElementById('lxmf-messages');
    var row = bubble && bubble.closest ? bubble.closest('.msg-row') : null;
    if (!row && container && msgData.id) {
        var target = _findRenderedMessageBubble(container, msgData.id);
        row = target && target.closest ? target.closest('.msg-row') : null;
    }
    var useSheet = isCompactLayout() && !opts.reactOnly;
    if (container) container.classList.add('is-acting');
    if (row) row.classList.add('is-pressed');
    var hoverBar = trigger && trigger.closest ? trigger.closest('.msg-hover') : null;

    var menu = document.createElement('div');
    menu.className = useSheet ? 'ui-dialog msg-sheet' : 'ui-menu ui-pop msg-menu' + (opts.reactOnly ? ' msg-react-pop' : '');
    menu.setAttribute('role', 'dialog');
    menu.setAttribute('aria-modal', 'false');
    menu.setAttribute('aria-label', opts.reactOnly ? 'Reactions' : 'Message actions');
    menu.addEventListener('mousedown', _preventMessageActionFocusSteal);
    menu.addEventListener('touchstart', _preventMessageActionFocusSteal, { passive: false });
    if (useSheet) menu.innerHTML = '<div class="ui-dialog-handle" aria-hidden="true"></div>';

    var reactBar = document.createElement('div');
    reactBar.className = 'msg-quick';
    reactBar.setAttribute('role', 'group');
    reactBar.setAttribute('aria-label', 'Reactions');
    MSG_QUICK_REACTIONS.forEach(function(em) {
        var btn = document.createElement('button');
        btn.type = 'button';
        btn.className = 'msg-quick-btn';
        btn.textContent = em;
        btn.setAttribute('data-message-action', 'reaction:' + em);
        var hasOwnReaction = _hasOwnReaction(msgData.id, em);
        btn.setAttribute('aria-pressed', hasOwnReaction ? 'true' : 'false');
        btn.setAttribute('aria-label', (hasOwnReaction ? 'Remove ' : 'Add ') + em + ' reaction');
        _bindMessageFocusPreservingActivation(btn, function(e) {
            _sendReactionForMessage(msgData, em, {
                focusExpected: _messageActivationExpectsFocus(e),
            });
        });
        reactBar.appendChild(btn);
    });

    var plusBtn = document.createElement('button');
    plusBtn.type = 'button';
    plusBtn.className = 'ui-icon-btn ui-icon-btn--sm msg-quick-more';
    plusBtn.innerHTML = RS.kit.icon('smile-plus');
    plusBtn.title = 'More reactions';
    plusBtn.setAttribute('aria-label', 'More reactions');
    plusBtn.setAttribute('data-message-action', 'reaction-more');
    _bindMessageFocusPreservingActivation(plusBtn, function() {
        var shouldRestoreComposer = _messageActionShouldPreserveComposer();
        // Capture before dismiss removes plusBtn from the DOM.
        var btnRect = plusBtn.getBoundingClientRect();
        _dismissContextMenu({ collapseHover: true });
        _restoreLxmfComposerKeyboard(shouldRestoreComposer);
        var tempAnchor = document.createElement('div');
        tempAnchor.style.cssText = 'position:fixed;left:' + btnRect.left + 'px;top:' + btnRect.top + 'px;width:' + btnRect.width + 'px;height:' + btnRect.height + 'px;pointer-events:none;';
        document.body.appendChild(tempAnchor);
        if (typeof EmojiPicker !== 'undefined') {
            var reactionPicker = new EmojiPicker({
                trigger: null,
                anchor: tempAnchor,
                container: document.getElementById('lxmf-chat-area'),
                onSelect: function(emoji) {
                    _sendReactionForMessage(msgData, emoji, { dismiss: false });
                    reactionPicker.close();
                    if (tempAnchor.parentNode) tempAnchor.parentNode.removeChild(tempAnchor);
                },
            });
            var origClose = reactionPicker.close.bind(reactionPicker);
            reactionPicker.close = function() {
                origClose();
                if (tempAnchor.parentNode) tempAnchor.parentNode.removeChild(tempAnchor);
            };
            reactionPicker.open();
        } else {
            if (tempAnchor.parentNode) tempAnchor.parentNode.removeChild(tempAnchor);
        }
    });
    reactBar.appendChild(plusBtn);
    menu.appendChild(reactBar);

    var actions = document.createElement('div');
    actions.className = useSheet ? 'ui-menu ui-menu--sheet msg-menu-items' : 'msg-menu-items';
    var itemClass = 'ui-menu-item';
    function add(btn, handler) {
        _bindMessageFocusPreservingActivation(btn, handler);
        actions.appendChild(btn);
    }
    var hasTop = false;

    if (_messageIsWaiting(msgData)) {
        hasTop = true;
        add(_messageMenuButton(itemClass + ' msg-ctx-try-now', 'try-now', 'refresh', 'Try now'), function() {
            var shouldRestoreComposer = _messageActionShouldPreserveComposer();
            _dismissContextMenu({ collapseHover: true });
            _tryWaitingLxmfMessage(msgData);
            _restoreLxmfComposerKeyboard(shouldRestoreComposer);
        });
        add(_messageMenuButton(itemClass + ' msg-ctx-stop-trying', 'stop-trying', 'x', 'Stop trying'), function() {
            var shouldRestoreComposer = _messageActionShouldPreserveComposer();
            _dismissContextMenu({ collapseHover: true });
            _stopWaitingLxmfMessage(msgData);
            _restoreLxmfComposerKeyboard(shouldRestoreComposer);
        });
    }

    if (_messageCanRetry(msgData)) {
        hasTop = true;
        add(_messageMenuButton(itemClass + ' msg-ctx-retry', 'retry', 'refresh', 'Retry…'), function() {
            var anchorBubble = state.bubble;
            _dismissContextMenu({ restoreFocus: false, collapseHover: true });
            _openRetryMethodPopover(msgData, anchorBubble);
        });
    }

    if (_messageCanShowInActivity(msgData) && typeof showMessageInActivity === 'function') {
        hasTop = true;
        add(_messageMenuButton(itemClass + ' msg-ctx-activity', 'show-activity', 'activity', 'Show in Activity'), function(e) {
            var focusChip = _messageActivationExpectsFocus(e);
            _dismissContextMenu({ restoreFocus: false, collapseHover: true });
            showMessageInActivity(msgData.id, {
                retryable: _messageCanRetry(msgData),
                focusChip: focusChip
            });
        });
    }

    if (hasTop) {
        var topSep = document.createElement('div');
        topSep.className = 'ui-menu-sep';
        topSep.setAttribute('role', 'separator');
        actions.appendChild(topSep);
    }

    add(_messageMenuButton(itemClass + ' msg-ctx-reply', 'reply', 'reply', 'Reply'), function() {
        _dismissContextMenu({ collapseHover: true });
        setReplyTarget(msgData);
        if (typeof haptic === 'function') haptic('light');
    });

    var content = bubble && bubble.querySelector ? bubble.querySelector('.msg-text') : null;
    var messageText = _messageCopyText(msgData, content);
    var hasText = !!(content && String(messageText || '').trim());
    if (hasText) {
        add(_messageMenuButton(itemClass + ' msg-ctx-copy', 'copy-message', 'copy', 'Copy'), function() {
            var shouldRestoreComposer = _messageActionShouldPreserveComposer();
            _dismissContextMenu({ collapseHover: true });
            _copyToClipboard(messageText).then(function(ok) {
                if (typeof showToast === 'function') {
                    showToast(ok ? 'Message copied' : 'Could not copy', ok ? 'toast-success' : 'toast-error', 1600);
                }
                if (typeof haptic === 'function') haptic(ok ? 'success' : 'warning');
                _restoreLxmfComposerKeyboard(shouldRestoreComposer);
            }).catch(function(err) {
                if (typeof showToast === 'function') {
                    showToast('Could not copy: ' + ((err && err.message) || 'unknown error'), 'toast-error', 3500);
                }
                if (typeof haptic === 'function') haptic('warning');
                _restoreLxmfComposerKeyboard(shouldRestoreComposer);
            });
        });
    }

    if (_messageSelectable(msgData)) {
        var isSelected = _messageSelectionActive() && _msgSelection.ids.indexOf(msgData.id) !== -1;
        add(_messageMenuButton(itemClass + ' msg-ctx-select', 'select', 'square-check', isSelected ? 'Deselect' : 'Select'), function() {
            _dismissContextMenu({ restoreFocus: false, collapseHover: true });
            if (isSelected) _toggleMessageSelection(msgData);
            else _selectMessage(msgData);
            if (typeof haptic === 'function') haptic('light');
        });
    }

    if (msgData.direction === 'outbound' && _messageIsPaper(msgData)) {
        add(_messageMenuButton(itemClass + ' msg-ctx-paper-qr', 'paper-qr', 'qr', 'Show QR code'), function() {
            _dismissContextMenu({ restoreFocus: false, collapseHover: true });
            _openPaperMessageShare(msgData);
        });
        add(_messageMenuButton(itemClass + ' msg-ctx-paper-link', 'copy-paper-link', 'copy', 'Copy paper link'), function() {
            var shouldRestoreComposer = _messageActionShouldPreserveComposer();
            _dismissContextMenu({ collapseHover: true });
            _paperUriForMessage(msgData).then(function(uri) {
                return _copyToClipboard(uri);
            }).then(function(ok) {
                showToast(ok ? 'Paper link copied' : 'Could not copy', ok ? 'toast-success' : 'toast-error', 1600);
                _restoreLxmfComposerKeyboard(shouldRestoreComposer);
            }).catch(function() {
                showToast('Could not copy', 'toast-error', 1800);
                _restoreLxmfComposerKeyboard(shouldRestoreComposer);
            });
        });
    }

    if (opts.linkUrl) {
        var linkUrl = opts.linkUrl;
        add(_messageMenuButton(itemClass + ' msg-ctx-copy-link', 'copy-link', 'copy', 'Copy link'), function() {
            var shouldRestoreComposer = _messageActionShouldPreserveComposer();
            _dismissContextMenu({ collapseHover: true });
            _copyToClipboard(linkUrl).then(function(ok) {
                showToast(ok ? 'Link copied' : 'Could not copy', ok ? 'toast-success' : 'toast-error', 1600);
                if (typeof haptic === 'function') haptic(ok ? 'success' : 'warning');
                _restoreLxmfComposerKeyboard(shouldRestoreComposer);
            }).catch(function() {
                showToast('Could not copy', 'toast-error', 1800);
                _restoreLxmfComposerKeyboard(shouldRestoreComposer);
            });
        });
    }

    var mediaAction = _messageMediaContextAction(msgData);
    if (mediaAction) {
        add(_messageMenuButton(itemClass + ' msg-ctx-media', 'media', mediaAction.icon === 'save' ? 'download' : 'copy', mediaAction.label), function() {
            var shouldRestoreComposer = _messageActionShouldPreserveComposer();
            _dismissContextMenu({ collapseHover: true });
            mediaAction.run().then(function(ok) {
                if (typeof haptic === 'function') haptic(ok ? 'success' : 'warning');
                _restoreLxmfComposerKeyboard(shouldRestoreComposer);
            }).catch(function(err) {
                if (typeof showToast === 'function') {
                    showToast((mediaAction.label.indexOf('Save') === 0 ? 'Could not save: ' : 'Could not copy: ') +
                        ((err && err.message) || 'unknown error'), 'toast-error', 3500);
                }
                if (typeof haptic === 'function') haptic('warning');
                _restoreLxmfComposerKeyboard(shouldRestoreComposer);
            });
        });
    }

    if (!opts.reactOnly) {
        if (!useSheet) {
            var sep = document.createElement('div');
            sep.className = 'ui-menu-sep';
            sep.setAttribute('role', 'separator');
            menu.appendChild(sep);
        }
        menu.appendChild(actions);
    }
    menu.addEventListener('keydown', function(e) {
        if (/^(Arrow(Up|Down|Left|Right)|Home|End|Tab)$/.test(e.key)) menu.classList.add('msg-action-keyboard');
        if (!/^(Arrow(Up|Down|Left|Right)|Home|End)$/.test(e.key)) return;
        var groups = [
            { panel: reactBar, items: Array.from(reactBar.querySelectorAll('button')), horizontal: true },
            { panel: actions, items: Array.from(actions.querySelectorAll('button')), horizontal: false },
        ].filter(function(group) { return group.panel.isConnected; });
        var target = _messageActionKeyTarget(groups, document.activeElement, e.key);
        if (!target) return;
        e.preventDefault();
        _focusMessageControl(target);
    });

    document.body.appendChild(menu);
    if (trigger && trigger.hasAttribute && trigger.hasAttribute('aria-expanded')) trigger.setAttribute('aria-expanded', 'true');
    var anchorEl = opts.anchor || (opts.reactOnly ? trigger : null);
    var anchorRect = anchorEl ? anchorEl.getBoundingClientRect()
        : (typeof x === 'number' && typeof y === 'number' && !opts.focusDialog ? { left: x, right: x, top: y, bottom: y } : null);
    var state = {
        menu: menu,
        reactPanel: reactBar,
        actionsPanel: actions,
        bubble: bubble,
        row: row,
        container: container,
        trigger: trigger,
        hoverBar: hoverBar,
        hoverAct: hoverBar && trigger ? trigger.getAttribute('data-hover-act') : '',
        anchorRect: anchorRect,
        sheet: useSheet,
        msgId: msgData.id,
        outbound: msgData.direction === 'outbound',
        ownerHash: _canonicalConversationHash(lxmfActiveContact),
        restoreFocusExpected: !!opts.focusDialog,
        preserveComposerKeyboard: preserveComposerKeyboard,
    };
    var repositionFrame = 0;
    state.reposition = function() {
        if (repositionFrame) return;
        repositionFrame = requestAnimationFrame(function() {
            repositionFrame = 0;
            if (_activeContextMenu !== state) return;
            var band = _messageActionBand();
            var r = state.bubble && state.bubble.isConnected ? state.bubble.getBoundingClientRect() : null;
            if (!r || r.bottom < band.top || r.top > band.bottom) {
                _dismissContextMenu({ restoreFocus: false });
                return;
            }
            if (state.anchorEl && state.anchorEl.isConnected) state.anchorRect = state.anchorEl.getBoundingClientRect();
            _layoutMessageActionPanels(state, false);
        });
    };
    state.anchorEl = anchorEl;
    _activeContextMenu = state;
    window.addEventListener('resize', state.reposition);
    if (container) container.addEventListener('scroll', state.reposition, { passive: true });
    if (window.visualViewport) {
        window.visualViewport.addEventListener('resize', state.reposition);
        window.visualViewport.addEventListener('scroll', state.reposition);
    }
    _layoutMessageActionPanels(state, true);
    if (opts.focusDialog) {
        _focusMessageControl(menu.querySelector('button'));
    }
}

function _handleMessageActionPointer(e) {
    if (_activeContextMenu) {
        var menu = _activeContextMenu.menu;
        if (menu && menu.contains(e.target)) return;
        // Its own trigger toggles the menu on click.
        var trigger = _activeContextMenu.trigger;
        if (trigger && trigger !== _activeContextMenu.bubble && trigger.contains && trigger.contains(e.target)) return;
        // The elevated bubble is the mobile native-selection surface. Keep
        // both it and the transcript DOM alive while a second hold begins.
        if (_messageActionOwnsText(_activeContextMenu.bubble, e.target)) return;
        if (_dismissContextMenu({ restoreFocus: false, flushDeferredRender: false })) {
            _scheduleDeferredConversationRenderAfterPointer();
        }
    }
}

var COPY_BLOCK_TAGS = /^(P|DIV|LI|PRE|BLOCKQUOTE|H1|H2|H3|UL|OL)$/;

function _copyTextOf(node, out) {
    if (node.nodeType === 3) { out.push(node.nodeValue); return; }
    if (node.nodeType !== 1 && node.nodeType !== 11) return;
    if (node.nodeType === 1) {
        if (node.tagName === 'BR') { out.push('\n'); return; }
        if (node.classList.contains('md-code-head') || node.tagName === 'BUTTON') return;
        if (node.classList.contains('md-spoiler') && !node.classList.contains('is-revealed')) {
            out.push('[Spoiler]');
            return;
        }
    }
    var block = node.nodeType === 1 && COPY_BLOCK_TAGS.test(node.tagName);
    if (block && out.length && !/\n$/.test(out[out.length - 1])) out.push('\n');
    for (var child = node.firstChild; child; child = child.nextSibling) _copyTextOf(child, out);
    if (block) out.push('\n');
}

// WebKit copies text under user-select:none and inert, so a selection that
// touches a covered spoiler is rewritten with [Spoiler] in its place.
function _handleSpoilerCopy(e) {
    var selection = window.getSelection ? window.getSelection() : null;
    if (!selection || selection.isCollapsed || !selection.rangeCount || !e.clipboardData) return;
    var range = selection.getRangeAt(0);
    var container = document.getElementById('lxmf-messages');
    if (!container || !container.contains(range.commonAncestorContainer)) return;
    var startEl = range.startContainer.nodeType === 1 ? range.startContainer : range.startContainer.parentElement;
    var fragment = range.cloneContents();
    var startCovered = startEl && startEl.closest && startEl.closest('.md-spoiler:not(.is-revealed)');
    if (!startCovered && !fragment.querySelector('.md-spoiler:not(.is-revealed)')) return;
    var parts = [];
    if (startCovered) parts.push('[Spoiler]');
    _copyTextOf(fragment, parts);
    var text = parts.join('').replace(/\u200B?(\[Spoiler\])(\[Spoiler\])*/g, '$1').replace(/\n{3,}/g, '\n\n').trim();
    e.clipboardData.setData('text/plain', text);
    e.preventDefault();
}

function _handleNativeMessageCopy() {
    if (!_mobileMessageActionsUseLongPress() || !_activeContextMenu) return false;
    var active = _activeContextMenu;
    if (!_messageSelectionIntersectsBubble(active.bubble)) return false;
    // Let the platform finish serializing the selected text before the
    // transcript DOM or selection is changed. Exact object identity prevents
    // this task from dismissing another message opened in the meantime.
    setTimeout(function() {
        if (_activeContextMenu !== active) return;
        _clearNativeMessageSelection();
        _dismissContextMenu({ restoreFocus: false });
    }, 0);
    return true;
}

document.addEventListener('pointerdown', _handleMessageActionPointer, true);
document.addEventListener('mousedown', _handleMessageActionPointer, true);
document.addEventListener('copy', _handleNativeMessageCopy, true);
document.addEventListener('copy', _handleSpoilerCopy, true);
document.addEventListener('keydown', function(e) {
    if (e.key !== 'Escape') return;
    if (_dismissContextMenu() ||
            (!(_msgSelection && _msgSelection.pending) && _clearMessageSelection())) {
        e.preventDefault();
    }
}, true);
document.addEventListener('click', _handleMessageSelectionClick, true);
document.addEventListener('keydown', function(e) {
    if ((e.key !== 'Delete' && e.key !== 'Backspace') || !_messageSelectionActive()) return;
    var target = e.target;
    if (target && (target.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName))) return;
    e.preventDefault();
    _deleteSelectedMessages();
});

window.RS = window.RS || {};
window.RS.closeMessageActionMenu = function() {
    if (_activeContextMenu) {
        _dismissContextMenu();
        return true;
    }
    return _clearMessageSelection();
};

RS.listen('messages_deleted', function(data) {
    if (data && data.hash && Array.isArray(data.ids)) _removeConversationMessages(data.hash, data.ids);
});

RS.listen('reaction_update', function(data) {
    if (data.message_id) {
        _msgReactions[data.message_id] = data.reactions || [];
        for (var i = 0; i < lxmfConversation.length; i++) {
            if (lxmfConversation[i].id === data.message_id) {
                lxmfConversation[i].reactions = data.reactions || [];
                renderConversation();
                break;
            }
        }
    }
});

(function() {
    var closeBtn = document.querySelector('.reply-preview-close');
    if (closeBtn) {
        closeBtn.addEventListener('click', function() {
            clearReplyTarget();
        });
    }
})();

RS.listen('lxmf_identity', function(data) {
    var nextIdentityHash = (data && (data.hash || data.identity_hash)) || null;
    if (_imageCacheIdentityHash && nextIdentityHash !== _imageCacheIdentityHash) {
        _clearImageBlobUrlCache();
    }
    _imageCacheIdentityHash = nextIdentityHash;
    lxmfIdentity = data;
    if (typeof syncActiveProfileStatusFromPayload === 'function') syncActiveProfileStatusFromPayload(data);
    // Re-render so we exclude self from the peer list.
    if (typeof renderConnectionsTable === 'function' && typeof PeersCache !== 'undefined' && PeersCache) {
        renderConnectionsTable(PeersCache.enriched());
    }
});

// Pre-warmed at startup; re-emitted on every conversation-touching command.
RS.listen('conversations_update', function(data) {
    _acceptConversations(data);
    if (typeof PeersCache !== 'undefined' && PeersCache &&
        typeof PeersCache.visibilityContextChanged === 'function') {
        PeersCache.visibilityContextChanged();
    }
});

if (typeof PeersCache !== 'undefined' && PeersCache && typeof PeersCache.subscribe === 'function') {
    PeersCache.subscribe(function() {
        _refreshRenderedConversationNames();
        _refreshRenderedConversationPresence();
        renderVoiceUi();
    });
}

RS.listen('contacts_update', function(data) {
    lxmfContacts = normalizeContactList(data);
    // peer_updated emissions handle PeersCache; no optimistic patch needed.
    if (typeof renderStandaloneContactList === 'function') renderStandaloneContactList();
    _refreshRenderedConversationNames();
    renderVoiceUi();
});

RS.listen('contact_blocked', function(data) {
    if (typeof showToast === 'function') showToast('User blocked', 'toast-success', 2000);
    // Optimistic removal; idempotent peer_removed event will follow.
    if (data && data.hash && typeof PeersCache !== 'undefined' && PeersCache) {
        PeersCache.applyRemoved(data.hash);
    }
    if (typeof refreshPeersList === 'function') refreshPeersList();
});

RS.listen('contact_unblocked', function(data) {
    if (typeof showToast === 'function') showToast('User unblocked', 'toast-success', 2000);
    if (typeof refreshPeersList === 'function') refreshPeersList();
});

RS.listen('conversation_update', function(data) {
    if (!data) return;
    var hash = _canonicalConversationHash(data.hash);
    var messages = _mergeConversationMessages(hash, data.messages || []);
    cacheSet(hash, messages);
    if (hash === _canonicalConversationHash(lxmfActiveContact)) {
        lxmfConversation = messages;
        renderConversation({ stickToBottom: true });
    }
});

RS.listen('lxmf_message', function(msg) {
    if (!msg) return;
    msg.source = _canonicalConversationHash(msg.source);
    msg.destination = _canonicalConversationHash(msg.destination);
    msg.thread = _canonicalConversationThread(msg.thread);
    var activeHash = _canonicalConversationHash(lxmfActiveContact);
    var appForeground = _lxmfAttentionForeground();
    var conversationVisible = _isConversationActivelyVisible(msg.source, msg.thread);
    var forActiveContact = msg.source === activeHash || msg.destination === activeHash;
    if (forActiveContact && msg.thread === lxmfActiveThread) {
        // Dedupe reconnect replays.
        var isDupe = msg.id && lxmfConversation.some(function(m) { return m.id === msg.id; });
        if (isDupe) return;
        var transcript = document.getElementById('lxmf-messages');
        if (msg.direction === 'inbound' && msg.id && transcript && !RS.chatScroll.isFollowing(transcript)) {
            _jumpNewIds.push(msg.id);
        }
        lxmfConversation.push(msg);
        _cacheActiveConversation();
        renderConversation({ stickToBottom: true });
        if (conversationVisible) {
            RS.invoke('mark_read', { hash: msg.source, thread: msg.thread || null }).catch(function() {});
        }
    } else if (forActiveContact) {
        _refreshActiveConversationThreads();
    }
    // Paper rows are filed by the user's own action, so they never alert.
    if (_messageIsPaper(msg) || _messageIsNote(msg)) return;
    if (appForeground && !conversationVisible) {
        var fromLabel = _messageSourceName(msg);
        var hasAudio = !!msg.audio;
        var hasAttachment = (msg.attachments && msg.attachments.length > 0) || msg.image;
        var toastMsg = hasAudio
            ? 'New voice message from ' + fromLabel
            : (hasAttachment
                ? 'New message with attachment from ' + fromLabel
                : 'New message from ' + fromLabel);
        var sourceHash = msg.source;
        var sourceThread = msg.thread;
        showToast(toastMsg, 'toast-action', 4000, function() {
            openConversationWith(sourceHash, { thread: sourceThread });
        });
    }
    // Tauri notifications are emitted by the runtime. This fallback is only
    // for a background browser, where the last selected chat is not visible.
    if (!window.__TAURI_INTERNALS__ && !appForeground && typeof rsNotify !== 'undefined') {
        var notifFrom = _messageSourceName(msg);
        var notifBody = _messagePlainContent(msg, !!msg.audio).substring(0, 120) || 'New message';
        rsNotify.send({
            title: 'Message from ' + notifFrom,
            body: notifBody,
            tag: 'lxmf-' + msg.source,
            onClick: function() { openConversationWith(msg.source, { thread: msg.thread }); }
        });
    }
});

document.addEventListener('rs-lifecycle-foreground-handled', function() {
    if (_isConversationActivelyVisible(lxmfActiveContact)) {
        RS.invoke('mark_read', {
            hash: _canonicalConversationHash(lxmfActiveContact),
            thread: lxmfActiveThread || null,
        }).catch(function() {});
    }
});

// The ✕ says it in an open chat; elsewhere one toast names the chat.
function _noticeUndeliveredMessage(destination, reason) {
    // Turning Keep trying off ends every waiting message at once; the marks show it.
    if (reason === 'keep_trying_off') return;
    var hash = _canonicalConversationHash(destination);
    if (!hash || !_lxmfAttentionForeground() || _isConversationActivelyVisible(hash, lxmfActiveThread)) return;
    var info = _conversationNameInfo(hash, null, false);
    var name = (info && info.name) || _hashFallbackName(hash);
    if (reason === 'wait_expired') {
        showToast('Message to ' + name + ' not delivered after 24 hours', 'toast-action', 4000, function() {
            openConversationWith(hash);
        });
        return;
    }
    showToast('Message to ' + name + ' not delivered', 'toast-action', 4000, function() {
        openConversationWith(hash);
    });
}

// The state a delivery step leaves a message in, or null when it changes nothing.
function _messageStateAfterStep(current, step) {
    var resolvedState = (step === 'error') ? 'failed' : step;
    // Paper is filed by the user and never arrives as a step.
    if (_isTerminalMessageState(resolvedState) && resolvedState !== 'paper') {
        // A proof outranks an earlier outcome; nothing else leaves one.
        var proofOutranks = resolvedState === 'delivered' &&
            current !== 'rejected' && current !== 'paper';
        return (!_isTerminalMessageState(current) || proofOutranks) ? resolvedState : null;
    }
    if (_isTerminalMessageState(current)) return null;
    if (resolvedState === 'waiting') return 'waiting';
    // A waiting message leaves only by the `sending` of a new attempt.
    if (current === 'waiting') return resolvedState === 'sending' ? 'sending' : null;
    return _isInFlightStep(resolvedState) ? resolvedState : null;
}

RS.listen('lxmf_step', function(data) {
    // Remap optimistic client_msg_id to canonical server msg_id.
    if (data.client_msg_id && data.msg_id) {
        for (var ri = 0; ri < lxmfConversation.length; ri++) {
            if (lxmfConversation[ri].id === data.client_msg_id) {
                lxmfConversation[ri].id = data.msg_id;
                break;
            }
        }
        if (data.step === 'cancelled') {
            delete _pendingLxmfCancelByClientId[data.client_msg_id];
        } else {
            _flushPendingLxmfCancel(data.client_msg_id, data.msg_id);
        }
    }
    if (data.step === 'delivered' || data.step === 'propagated' || data.step === 'failed' || data.step === 'cancelled' || data.step === 'timeout' || data.step === 'error' || data.step === 'rejected') {
        var resolvedState = (data.step === 'error') ? 'failed' : data.step;
        var matched = false;
        var eventMsgId = data.msg_id || data.client_msg_id;
        lxmfConversation.forEach(function(msg) {
            if (eventMsgId && msg.id === eventMsgId) {
                if (_messageStateAfterStep(msg.state, data.step)) {
                    msg.state = resolvedState;
                    if (data.rtt_ms) msg.rtt = data.rtt_ms;
                    if (data.method) msg.delivery_method = data.method;
                }
                matched = true;
            }
        });
        // Fallback for legacy events with no msg_id.
        if (!matched && !eventMsgId) {
            for (var i = lxmfConversation.length - 1; i >= 0; i--) {
                var msg = lxmfConversation[i];
                if (msg.state === 'sending' || msg.state === 'sent') {
                    msg.state = resolvedState;
                    break;
                }
            }
        }
        _cacheActiveConversation();
        renderConversation();
        if (data.client_msg_id) delete _pendingLxmfCancelByClientId[data.client_msg_id];
    }
    if (_isInFlightStep(data.step)) {
        lxmfConversation.forEach(function(msg) {
            if (data.msg_id && msg.id === data.msg_id) {
                if (_messageStateAfterStep(msg.state, data.step)) msg.state = data.step;
                if (data.method) msg.delivery_method = data.method;
            }
        });
        _cacheActiveConversation();
        renderConversation();
    }
    // `waiting` parks a message on its way; the `sending` of a new attempt takes it out.
    if (data.step === 'waiting' || data.step === 'sending') {
        var parkedOrWoken = false;
        lxmfConversation.forEach(function(msg) {
            if (data.msg_id && msg.id === data.msg_id && _messageStateAfterStep(msg.state, data.step)) {
                msg.state = data.step;
                if (data.method) msg.delivery_method = data.method;
                delete msg.delivery_progress;
                delete msg.delivery_link_id;
                delete msg.delivery_representation;
                parkedOrWoken = true;
            }
        });
        if (parkedOrWoken) {
            _cacheActiveConversation();
            renderConversation();
        }
    }

    if (data.step === 'failed' || data.step === 'timeout') {
        _noticeUndeliveredMessage(data.destination, data.reason);
    }
    if (data.step === 'error') {
        showToast(data.message || 'Send error', 'toast-error', 5000);
    }
    if (data.step === 'rejected') {
        showToast(data.message || 'The recipient rejected the message', 'toast-error', 5000);
    }
});

RS.listen('lxmf_delivery_progress', function(data) {
    if (data.client_msg_id && data.msg_id) {
        for (var ri = 0; ri < lxmfConversation.length; ri++) {
            if (lxmfConversation[ri].id === data.client_msg_id) {
                lxmfConversation[ri].id = data.msg_id;
                break;
            }
        }
        _flushPendingLxmfCancel(data.client_msg_id, data.msg_id);
    }
    var changed = false;
    lxmfConversation.forEach(function(msg) {
        if (data.msg_id && msg.id === data.msg_id) {
            if (typeof data.progress === 'number') msg.delivery_progress = data.progress;
            if (data.link_id) msg.delivery_link_id = data.link_id;
            if (data.representation) msg.delivery_representation = data.representation;
            if (data.method) msg.delivery_method = data.method;
            if (_isInFlightStep(data.step) && _messageStateAfterStep(msg.state, data.step)) {
                msg.state = data.step;
            }
            changed = true;
        }
    });
    if (changed) {
        _cacheActiveConversation();
        renderConversation();
    }
});

RS.listen('voice_call_update', _voiceHandleUpdate);
RS.listen('voice_incoming_call', function(data) {
    _voiceCancelMemoForCall().then(function() {
        _voiceHandleUpdate(Object.assign({ type: 'incoming' }, data || {}));
    });
});

document.addEventListener('rs-audio-playback-ready', _voiceSyncRingtone);
document.addEventListener('visibilitychange', function() {
    if (!document.hidden) _voiceSyncRingtone();
});

RS.listen('contact_added', function(data) {
    var addedName = data && data.display_name;
    showToast(addedName ? 'Contact added: ' + addedName : 'Contact added', 'toast-success', 3000);
    if (typeof renderStandaloneContactList === 'function') renderStandaloneContactList();
    if (typeof refreshPeersList === 'function') refreshPeersList();
});

RS.listen('contact_error', function(data) {
    showToast(data.error || 'Could not update contact', 'toast-error', 4000);
});

function showContactAbout(hash) {
    var reach = _peerInfo(hash);
    var nameInfo = _conversationNameInfo(hash, null, false);
    var name = nameInfo.isHash ? '' : nameInfo.name;
    var contact = _lookupContactRecord(hash);
    var profileStatus = reach && typeof ratspeakProfileStatusText === 'function'
        ? ratspeakProfileStatusText(reach)
        : (reach && ratspeakSupportsFeatures(reach) && reach.profile_status ? reach.profile_status : '');
    var msgCount = lxmfConversation ? lxmfConversation.length : 0;

    var modal = document.getElementById('node-modal');
    if (!modal) return;
    document.getElementById('modal-title').textContent = 'About';
    var body = modal.querySelector('.bottom-sheet-body');

    var html = '<div class="about-hero">' +
        peerAvatarHtml('about-hero-avatar', hash, 56, reach) +
        peerNameHtml('about-hero-name', escapeHtml(name)) +
        peerAddressButtonHtml(hash) +
    '</div>';

    html += '<div class="about-section">';
    if (profileStatus) {
        html += '<div class="about-row">' +
            '<span class="about-row-label">Status</span>' +
            '<span class="about-row-value">' + escapeHtml(profileStatus) + '</span>' +
        '</div>';
    }
    html += '<div class="about-row">' +
        '<span class="about-row-label">Messages</span>' +
        (msgCount > 0
            ? '<a href="#" class="about-messages-link">' + msgCount + ' \u00b7 View \u2192</a>'
            : '<span class="about-row-value about-row-value-muted">0</span>') +
    '</div>';
    html += '</div>';

    html += peerDetailsHtml('about-detail-rows', reach, { trust: contact ? contact.trust : '' });
    html += peerActionRowsHtml(!!contact);

    body.innerHTML = html;
    syncPeerDetails(body);
    var overlay = document.getElementById('node-modal-overlay');
    modal.classList.add('open');
    if (overlay) overlay.classList.add('active');

    // CSP blocks inline onclick — wire here.
    var msgLink = body.querySelector('.about-messages-link');
    if (msgLink) {
        msgLink.addEventListener('click', function(ev) {
            ev.preventDefault();
            ev.stopPropagation();
            if (typeof closeNodeModal === 'function') closeNodeModal();
            var msgs = document.getElementById('lxmf-messages');
            if (msgs) msgs.scrollTop = 0;
        });
    }
    wirePeerActionRows(body, hash, { name: name, before: closeNodeModal });
}

function openConversationWith(hash, options) {
    hash = _canonicalConversationHash(hash);
    if (_ghostConversationHash && _ghostConversationHash !== hash) {
        _removeGhostRow();
    }
    var input = document.getElementById('lxmf-input');
    _saveActiveDraft(input);
    if (typeof switchView === 'function') switchView('message');
    var thread = options && options.thread !== undefined
        ? options.thread
        : (hash === _canonicalConversationHash(lxmfActiveContact) ? undefined : _latestConversationThread(hash));
    hash = _activateConversation(hash, 'navigation', thread).hash;
    _restoreActiveDraft(input);
    var loaded = _loadConversation(hash);
    _ensureGhostRow(hash);
    var input = document.getElementById('lxmf-input');
    if (input && (!options || options.focusComposer !== false)) input.focus();
    if (isCompactLayout()) {
        RS.viewStack.push('chat-detail', { meta: { contactHash: hash } });
        history.pushState({ view: 'message', detail: true }, '', '#message');
    }
    return loaded;
}

// Opens a conversation and flashes one message once its history has loaded.
function openConversationAtMessage(hash, messageId, timestamp, thread) {
    hash = _canonicalConversationHash(hash);
    var options = { focusComposer: false };
    if (thread !== undefined) options.thread = thread || '';
    Promise.resolve(openConversationWith(hash, options)).then(function() {
        setTimeout(function() {
            if (_canonicalConversationHash(lxmfActiveContact) !== hash) return;
            var ts = Number(timestamp);
            var msg = lxmfConversation.find(function(m) { return messageId && m.id === messageId; }) ||
                lxmfConversation.slice().reverse().find(function(m) {
                    return ts && Math.abs(Number(m.timestamp) - ts) < 1;
                });
            var container = document.getElementById('lxmf-messages');
            var bubble = msg && _findRenderedMessageBubble(container, msg.id);
            if (!bubble) return;
            bubble.scrollIntoView({ behavior: 'smooth', block: isCompactLayout() ? 'nearest' : 'center' });
            bubble.classList.add('msg-highlight');
            setTimeout(function() { bubble.classList.remove('msg-highlight'); }, 1500);
        }, 120);
    });
}
window.openConversationAtMessage = openConversationAtMessage;

// New message: one sheet that takes a contact's name or a pasted LXMF address.
var _newMessageSheet = null;
var LXMF_ADDRESS_HEX = /^[0-9a-f]{32}$/;

function _newMessageAddress(value) {
    var hex = String(value || '').replace(/^lxma?:\/\//i, '').replace(/[\s<>:]/g, '').toLowerCase();
    return LXMF_ADDRESS_HEX.test(hex) ? hex : '';
}

function _newMessagePersonRow(hash, name, sub) {
    var presence = _conversationPresence(hash);
    var nameInfo = name ? { name: name, isHash: false } : _conversationNameInfo(hash, null, false);
    var title = nameInfo.isHash
        ? '<span class="msg-title-hash">' + escapeHtml(nameInfo.name) + '</span>'
        : ratspeakDisplayNameHtml(nameInfo.name, hash);
    return '<li><button type="button" class="ui-row" data-hash="' + escapeHtml(hash) + '">' +
        RS.kit.avatar(hash, 'row', presence.active ? 'active' : null, presence.heard) +
        '<span class="ui-row-main"><span class="ui-row-top"><span class="ui-row-title">' + title + '</span></span>' +
        '<span class="ui-row-sub"><span class="t' + (sub ? '' : ' ui-hash') + '">' + escapeHtml(sub || RS.kit.hash.short(hash)) + '</span></span></span></button></li>';
}

function _newMessageListHtml(query) {
    var q = String(query || '').trim().toLowerCase();
    var address = _newMessageAddress(query);
    var matches = function(hash, name) {
        return !q || String(name || '').toLowerCase().indexOf(q) !== -1 || String(hash || '').indexOf(q.replace(/\s+/g, '')) !== -1;
    };
    var html = '';
    if (!q) {
        html += '<ul class="ui-list ui-phone-only"><li><button type="button" class="ui-row ui-row--one" data-new-action="scan">' +
            '<span class="ui-tile">' + RS.kit.icon('qr') + '</span><span class="ui-row-main"><span class="ui-row-top"><span class="ui-row-title">Scan a contact card</span></span></span>' +
            RS.kit.icon('chevron-right') + '</button></li></ul>';
    }
    if (address) {
        html += '<ul class="ui-list" aria-label="LXMF address">' + _newMessagePersonRow(address, '', 'LXMF address') + '</ul>';
    }
    var recent = (lxmfConversations || []).slice().sort(function(a, b) {
        return (b.timestamp || 0) - (a.timestamp || 0);
    }).filter(function(c) {
        return matches(c.hash, _conversationNameInfo(c.hash, c.display_name, c.is_contact).name);
    }).slice(0, 3);
    if (recent.length) {
        html += '<ul class="ui-list" aria-label="Recent"><li class="ui-caption">Recent</li>' + recent.map(function(c) {
            var info = _conversationNameInfo(c.hash, c.display_name, c.is_contact);
            return _newMessagePersonRow(c.hash, info.isHash ? '' : info.name);
        }).join('') + '</ul>';
    }
    var contacts = (lxmfContacts || []).filter(function(c) {
        return c && c.hash && matches(c.hash, c.display_name);
    }).sort(function(a, b) {
        return (a.display_name || '').localeCompare(b.display_name || '');
    });
    if (contacts.length) {
        html += '<ul class="ui-list" aria-label="Contacts"><li class="ui-caption">Contacts</li>' + contacts.map(function(c) {
            return _newMessagePersonRow(c.hash, c.display_name || 'Anonymous');
        }).join('') + '</ul>';
    }
    if (q && !address && !recent.length && !contacts.length) {
        var partial = /^[0-9a-f\s]+$/i.test(q) && q.replace(/\s+/g, '').length >= 8;
        html += '<div class="ui-empty"><h3 class="ui-empty-title">No contacts match “' + escapeHtml(query.trim()) + '”</h3>' +
            '<p class="ui-empty-body">' + (partial ? 'An LXMF address has 32 characters.' : 'Search checks names and addresses, or paste an LXMF address.') + '</p></div>';
    } else if (!q && !recent.length && !contacts.length) {
        html += '<div class="ui-empty"><p class="ui-empty-body">Paste an LXMF address to message anyone.</p></div>';
    }
    return html;
}

function openNewMessageSheet() {
    if (_newMessageSheet) return;
    var built = RS.kit.dialog({
        title: 'New message',
        className: 'msg-picker',
        focus: false,
        sheet: '<div class="msg-picker-search"><div class="ui-search" role="search">' + RS.kit.icon('search') +
            '<input type="search" placeholder="Name or LXMF address" aria-label="Search contacts, or paste an LXMF address" autocorrect="off" autocapitalize="none" spellcheck="false"></div></div>' +
            '<div class="msg-picker-list ui-scroll">' + _newMessageListHtml('') + '</div>',
        onClose: function() { _newMessageSheet = null; }
    });
    _newMessageSheet = built;
    var input = built.dialog.querySelector('input[type="search"]');
    var list = built.dialog.querySelector('.msg-picker-list');
    if (RS.composer && RS.composer.bindTypingPolicy) RS.composer.applyTypingPolicy(input);
    input.addEventListener('input', function() { list.innerHTML = _newMessageListHtml(input.value); });
    input.addEventListener('keydown', function(e) {
        if (e.key !== 'Enter' || e.isComposing) return;
        e.preventDefault();
        var first = list.querySelector('.ui-row[data-hash]');
        if (first) first.click();
    });
    list.addEventListener('click', function(e) {
        var row = e.target.closest && e.target.closest('.ui-row');
        if (!row) return;
        built.close();
        if (row.getAttribute('data-new-action') === 'scan') {
            if (typeof openContactQrScanner === 'function') openContactQrScanner();
            return;
        }
        openConversationWith(row.getAttribute('data-hash'));
    });
    // A phone opens on the list; the keyboard comes with the first tap in the field.
    if (!isCompactLayout()) _focusMessageControl(input);
    history.pushState({ view: currentView, newMessage: true }, '', '#' + currentView);
}

function closeFabContactPicker() {
    if (_newMessageSheet) _newMessageSheet.close();
}

// Compose: New message, and with Paper messages on, Open a paper message.
function _composeItems() {
    return [
        { label: 'New message', icon: 'message-plus', onSelect: openNewMessageSheet },
        { label: 'Open a paper message', icon: 'qr', onSelect: function() { if (typeof openPaperDecoder === 'function') openPaperDecoder(); } }
    ];
}

function _paperMessagesOn() {
    return document.documentElement.dataset.paperMessages === 'on' && typeof openPaperDecoder === 'function';
}

function openComposeMenu(trigger) {
    trigger.setAttribute('aria-haspopup', _paperMessagesOn() ? 'menu' : 'dialog');
    if (!_paperMessagesOn()) {
        openNewMessageSheet();
        return;
    }
    RS.kit.menu(trigger, { title: 'New', items: _composeItems() });
}

// Phone: the floating compose button opens its menu above a scrim, and turns into Close.
function initComposeFab() {
    var fab = document.getElementById('lxmf-send-fab');
    var wrap = document.getElementById('lxmf-fab-dial');
    if (!fab || !wrap) return;
    function setOpen(open) {
        wrap.classList.toggle('is-open', open);
        fab.setAttribute('aria-expanded', open ? 'true' : 'false');
        fab.setAttribute('aria-label', open ? 'Close' : 'New message');
        fab.title = open ? 'Close' : 'New message';
        fab.innerHTML = RS.kit.icon(open ? 'x' : 'compose');
    }
    window._closeFabDial = function() { RS.kit.closeMenu(); };
    RS.gestures.bindViewFabClick(fab, function(e) {
        e.stopPropagation();
        if (!_paperMessagesOn()) {
            openNewMessageSheet();
            return;
        }
        var handle = RS.kit.menu(fab, {
            label: 'New', items: _composeItems(), placement: 'top', sheet: false, cls: 'msg-fab-menu',
            onClose: function() { setOpen(false); }
        });
        setOpen(!!handle);
    });
}

(function() {
    var dropZone = document.getElementById('lxmf-chat-area');
    if (!dropZone) return;
    var _dragCounter = 0;

    dropZone.addEventListener('dragenter', function(e) {
        if (!e.dataTransfer.types || e.dataTransfer.types.indexOf('Files') === -1) return;
        e.preventDefault();
        e.stopPropagation();
        _dragCounter++;
        if (_dragCounter === 1) dropZone.classList.add('drag-over');
    });
    dropZone.addEventListener('dragover', function(e) {
        e.preventDefault();
        e.stopPropagation();
        e.dataTransfer.dropEffect = 'copy';
    });
    dropZone.addEventListener('dragleave', function(e) {
        e.preventDefault();
        e.stopPropagation();
        _dragCounter--;
        if (_dragCounter <= 0) { _dragCounter = 0; dropZone.classList.remove('drag-over'); }
    });
    dropZone.addEventListener('drop', function(e) {
        e.preventDefault();
        e.stopPropagation();
        _dragCounter = 0;
        dropZone.classList.remove('drag-over');
        if (!lxmfActiveContact) { showPreConditionToast('Select a conversation first'); return; }
        var files = e.dataTransfer.files;
        if (!files || files.length === 0) return;
        if (files.length > 1) showToast('Only one file can be attached at a time', 'toast-warning', 3000);
        handleFileSelected({ files: [files[0]], value: '' });
    });
})();

(function() {
    var chatArea = document.getElementById('lxmf-chat-area');
    if (!chatArea) return;
    function attachPastedFile(file) {
        if (!lxmfActiveContact) { showPreConditionToast('Select a conversation first'); return; }
        // Rename pasted 'image.png' with a timestamp to avoid backend collisions.
        if (typeof File !== 'undefined' && (file.name === 'image.png' || !file.name)) {
            var ts = new Date().toISOString().replace(/[:.]/g, '-').slice(0, 19);
            var ext = (file.type && file.type.split('/')[1]) || 'png';
            file = new File([file], 'paste-' + ts + '.' + ext, { type: file.type });
        }
        handleFileSelected({ files: [file], value: '' });
    }

    chatArea.addEventListener('paste', function(e) {
        var data = e.clipboardData;
        var items = data && data.items;
        if (!items) return;
        var file = null;
        for (var i = 0; i < items.length; i++) {
            if (items[i].kind === 'file') {
                file = items[i].getAsFile();
                break;
            }
        }
        if (file) {
            e.preventDefault();
            attachPastedFile(file);
            return;
        }
        if (!_rsPastesImagesAsNothing() || data.getData('text/plain')) return;
        e.preventDefault();
        _rsReadNativeClipboardImage().then(function(file) {
            if (file) attachPastedFile(file);
        }).catch(function() {
            showToast('Could not paste the image', 'toast-error', 3500);
        });
    });
})();

// Add or rename a contact from the conversation.
function _promptContactName(hash, contact) {
    rsPrompt({
        message: contact ? 'New name:' : 'Contact name:',
        defaultValue: contact ? contact.display_name || '' : '',
        placeholder: 'Display name',
        inputClass: 'rs-dialog-input--text'
    }).then(function(newName) {
        if (newName !== null) {
            RS.invokeOrToast('add_contact', { args: { hash: hash, display_name: newName.trim() || null } }, contact ? 'Could not rename contact' : 'Could not add contact');
        }
    });
}

function _startGameWith(hash) {
    if (typeof window.openNewGame === 'function') window.openNewGame({ contactHash: hash });
}

// The conversation's Options: a menu on desktop, a sheet on a phone.
function openChatHeaderDropdown(triggerEl) {
    if (!lxmfActiveContact) return;
    var hash = lxmfActiveContact;
    var contact = _lookupContactRecord(hash);
    var info = _conversationNameInfo(hash, null, false);
    var first = info.isHash ? '' : String(info.name || '').trim().split(/\s+/)[0];
    var thread = RS.threads ? RS.threads.current() : null;
    var items = [];
    if (isCompactLayout() && typeof window.openNewGame === 'function') {
        items.push({ label: 'Challenge to a game', icon: 'games', onSelect: function() { _startGameWith(hash); } });
    }
    items.push({ label: 'New conversation', icon: 'message-plus', onSelect: function() { if (RS.threads) RS.threads.startNew(); } });
    if (thread && thread.thread) {
        items.push({ label: 'Rename conversation', icon: 'tag', onSelect: function() { RS.threads.rename(thread); } });
    }
    items.push(
        { label: contact ? 'Rename contact' : 'Add contact', icon: contact ? 'edit' : 'user-plus', onSelect: function() { _promptContactName(hash, contact); } },
        { label: first ? 'About ' + first : 'About this person', icon: 'info', onSelect: function() { showContactAbout(hash); } },
        { label: 'Copy LXMF address', icon: 'copy', onSelect: function() {
            RS.copyText(hash).then(function(ok) {
                if (ok) showCopyConfirmationToast('LXMF address');
                else showToast('Could not copy', 'toast-error', 1500);
            });
        } },
        { separator: true },
        { label: 'Delete conversation', icon: 'trash', danger: true, onSelect: function() {
            // With several conversations this deletes only the open one.
            if (thread && RS.threads.hasExtra()) {
                RS.threads.remove(thread);
                return;
            }
            showConversationDeleteDialog(hash, info.name);
        } }
    );
    RS.kit.menu(triggerEl || document.getElementById('chat-header-menu-btn'), { title: info.name, label: 'Conversation options', items: items });
}

document.addEventListener('DOMContentLoaded', function() {
    RS.invoke('api_lxmf_limits').then(function(limits) {
        if (limits && typeof limits === 'object') {
            Object.assign(lxmfLimits, limits);
        }
    }).catch(function() {});

    // CSP blocks inline onchange.
    var fileInput = document.getElementById('lxmf-file-input');
    if (fileInput) fileInput.addEventListener('change', function() { handleFileSelected(this); });
    var photosInput = document.getElementById('lxmf-photos-input');
    if (photosInput) photosInput.addEventListener('change', function() { handleFileSelected(this); });
    var cameraInput = document.getElementById('lxmf-camera-input');
    if (cameraInput) cameraInput.addEventListener('change', function() { handleFileSelected(this); });
    var videoInput = document.getElementById('lxmf-video-input');
    if (videoInput) videoInput.addEventListener('change', function() { handleFileSelected(this); });

    var lxstCallBtn = document.getElementById('lxst-call-btn');
    if (lxstCallBtn) {
        lxstCallBtn.addEventListener('click', function(e) {
            e.preventDefault();
            e.stopPropagation();
            _voiceBlockMobileNavigation(280);
            if (lxmfActiveContact) _voiceRunPrimaryAction(lxmfActiveContact);
        });
    }
    ['lxst-call-answer-btn', 'lxst-call-global-answer-btn'].forEach(function(id) {
        var btn = document.getElementById(id);
        if (btn) btn.addEventListener('click', _voiceAnswerCall);
    });
    ['lxst-call-reject-btn', 'lxst-call-global-reject-btn'].forEach(function(id) {
        var btn = document.getElementById(id);
        if (btn) btn.addEventListener('click', _voiceRejectCall);
    });
    ['lxst-call-hangup-btn', 'lxst-call-global-hangup-btn'].forEach(function(id) {
        var btn = document.getElementById(id);
        if (btn) btn.addEventListener('click', _voiceHangupCall);
    });
    ['lxst-call-mute-btn', 'lxst-call-global-mute-btn'].forEach(function(id) {
        var btn = document.getElementById(id);
        if (btn) btn.addEventListener('click', _voiceToggleMute);
    });
    ['lxst-call-speaker-btn', 'lxst-call-global-speaker-btn'].forEach(function(id) {
        var btn = document.getElementById(id);
        if (btn) btn.addEventListener('click', _voiceToggleSpeaker);
    });
    _voiceWireHangupProximity('lxst-call-strip', 'lxst-call-hangup-btn');
    _voiceWireHangupProximity('lxst-call-global', 'lxst-call-global-hangup-btn');
    RS.invoke('voice_status').then(function(status) {
        lxstVoiceState.available = true;
        lxstVoiceState.running = !!(status && status.running);
        if (status && typeof status.microphone_muted === 'boolean') {
            lxstVoiceState.microphoneMuted = status.microphone_muted;
        }
        if (status && status.snapshot) {
            _voiceHandleUpdate(status.snapshot);
        } else {
            renderVoiceUi();
        }
    }).catch(function() {
        lxstVoiceState.available = false;
        renderVoiceUi();
    });

    var msgContainer = document.getElementById('lxmf-messages');
    if (msgContainer) RS.chatScroll.wire(msgContainer);
    if (msgContainer && typeof isMobile === 'function' && isMobile()) {
        msgContainer.addEventListener('touchstart', function(e) {
            if (e.defaultPrevented) return;
            var t = e.target;
            // Don't blur for taps on the compose bar or interactive controls — only
            // for taps on message body / list whitespace.
            if (t && t.closest && (
                t.closest('.cmp') ||
                t.closest('button, a, input, textarea, select, [role="button"], [role="menuitem"]') ||
                (t.closest('.msg-bubble') && _shouldPreserveLxmfComposerKeyboard())
            )) return;
            var active = document.activeElement;
            if (active && (active.tagName === 'INPUT' || active.tagName === 'TEXTAREA')) {
                active.blur();
            }
        }, { passive: true });
    }

    var sendBtn = document.getElementById('send-msg-btn');

    var attachBtn = document.getElementById('attach-file-btn');

    function openComposeEmojiPicker() {
        if (typeof EmojiPicker === 'undefined' || !attachBtn) return;
        var picker = new EmojiPicker({
            anchor: attachBtn,
            container: document.getElementById('lxmf-chat-area') || document.body,
            onSelect: function(emoji) {
                var input = document.getElementById('lxmf-input');
                if (!input) return;
                var start = input.selectionStart;
                var end = input.selectionEnd;
                input.value = input.value.substring(0, start) + emoji + input.value.substring(end);
                input.selectionStart = input.selectionEnd = start + emoji.length;
                input.focus();
                input.dispatchEvent(new Event('input'));
            }
        });
        picker.open();
    }

    // Hold Send: choose how this one message goes. A tap always sends Automatic.
    function openSendMethodPopover() {
        if (!sendBtn || !lxmfActiveContact) return;
        var hasRelay = _offlineInboxAvailable();
        var input = document.getElementById('lxmf-input');
        var wasFocused = !!(input && document.activeElement === input);
        var hasText = !!(input && input.value.trim());
        RS.kit.menu(sendBtn, { title: 'Send by', caption: 'Send this message by', hints: true, placement: 'top', keepFocus: wasFocused, items: [
            { label: 'Opportunistic', icon: 'opportunistic', hint: 'Single packets. Best for short text.', onSelect: function() { sendLxmfMessage('opportunistic'); } },
            { label: 'Direct', icon: 'direct', hint: 'Opens a link first. Best for long text and files.', onSelect: function() { sendLxmfMessage('direct'); } },
            { label: 'Offline Inbox', icon: 'envelope', disabled: !hasRelay,
                hint: hasRelay ? 'Held by an Offline Inbox node until they collect it.' : 'Turn on Offline Inbox in Settings to use it.',
                onSelect: function() { sendLxmfMessage('propagated'); } },
            { separator: true },
            { label: 'Paper', icon: 'qr', hint: 'A QR code or link you hand over yourself.', disabled: !hasText, onSelect: sendLxmfPaperMessage },
        ], onClose: function() {
            if (wasFocused && input && document.activeElement !== input) _focusLxmfComposerInput(input);
        } });
    }

    if (sendBtn) {
        // Send button gesture wiring. Tap = sendLxmfMessage('auto'), 500ms hold = method popover.
        // Both paths must preserve textarea focus on iOS so the soft keyboard stays open.
        // Implementation:
        //   - Non-passive touchstart preventDefault blocks iOS synthetic mouse events that
        //     would otherwise transfer first-responder away from the textarea (and collapse
        //     the keyboard before the hold timer fires).
        //   - mousedown preventDefault is the desktop equivalent for keyboard preservation.
        //   - Because preventDefault on touchstart suppresses the synthetic click on iOS,
        //     the tap-to-send path is dispatched from touchend instead, with a short
        //     suppressClick window guarding against any platform that still synthesizes click.
        var holdTimer = null;
        var holdFired = false;
        var didDrift = false;
        var startX = 0, startY = 0;
        var suppressClick = false;
        var moveCancelPx = (RS.gestures && RS.gestures.LONG_PRESS_MOVE_CANCEL_PX) || 20;
        var moveCancelSq = moveCancelPx * moveCancelPx;
        var holdMs = (RS.gestures && RS.gestures.LONG_PRESS_SEND_MS) || 500;

        function _startHold() {
            holdFired = false;
            didDrift = false;
            if (holdTimer) clearTimeout(holdTimer);
            holdTimer = setTimeout(function() {
                holdFired = true;
                holdTimer = null;
                if (typeof haptic === 'function') haptic('medium');
                openSendMethodPopover();
            }, holdMs);
        }
        function _clearHold() {
            if (holdTimer) clearTimeout(holdTimer);
            holdTimer = null;
        }
        function _armSuppressClick() {
            suppressClick = true;
            setTimeout(function() { suppressClick = false; }, 500);
        }

        sendBtn.addEventListener('touchstart', function(e) {
            e.preventDefault();
            _captureLxmfSendFocusState();
            var t = e.touches && e.touches[0];
            if (t) { startX = t.clientX; startY = t.clientY; }
            _startHold();
        }, { passive: false });

        sendBtn.addEventListener('touchmove', function(e) {
            if (!holdTimer || holdFired) return;
            var t = e.touches && e.touches[0];
            if (!t) return;
            var dx = t.clientX - startX;
            var dy = t.clientY - startY;
            if (dx * dx + dy * dy > moveCancelSq) {
                didDrift = true;
                _clearHold();
            }
        }, { passive: true });

        sendBtn.addEventListener('touchend', function() {
            if (holdFired) {
                _armSuppressClick();
                holdFired = false;
                return;
            }
            var hadTimer = !!holdTimer;
            _clearHold();
            if (!didDrift && hadTimer) {
                _armSuppressClick();
                sendLxmfMessage('auto');
            }
        });

        sendBtn.addEventListener('touchcancel', function() {
            _clearHold();
            holdFired = false;
        });

        sendBtn.addEventListener('mousedown', function(e) {
            e.preventDefault();
            _captureLxmfSendFocusState();
            _startHold();
        });
        ['mouseup', 'mouseleave'].forEach(function(ev) {
            sendBtn.addEventListener(ev, function() {
                if (!holdFired) _clearHold();
            });
        });

        sendBtn.addEventListener('contextmenu', function(e) {
            e.preventDefault();
            _clearHold();
            openSendMethodPopover();
        });

        sendBtn.addEventListener('click', function(e) {
            if (suppressClick || holdFired) {
                e.preventDefault();
                e.stopPropagation();
                holdFired = false;
                return;
            }
            sendLxmfMessage('auto');
        });
    }

    if (attachBtn) {
        attachBtn.addEventListener('click', function(e) {
            e.stopPropagation();
            // iOS gets the native action sheet for a bare <input type="file">.
            // Android adds Camera, Video, Photos, and File (WebChromeClient lacks a camera).
            // Desktop adds Emoji, since the OS keyboard doesn't help there.
            if (isIOS()) {
                triggerFileAttachment();
                return;
            }
            var input = document.getElementById('lxmf-input');
            var items = isAndroid() ? [
                { label: 'Camera', icon: 'camera', onSelect: triggerCameraAttachment },
                { label: 'Video', icon: 'video', onSelect: triggerVideoAttachment },
                { label: 'Photos', icon: 'image', onSelect: triggerPhotosAttachment },
                { label: 'File', icon: 'file', onSelect: triggerFileAttachment },
            ] : [
                { label: 'Emoji', icon: 'smile', onSelect: openComposeEmojiPicker },
                { label: 'File or image', icon: 'file', onSelect: triggerFileAttachment },
            ];
            RS.kit.menu(attachBtn, {
                title: 'Attach', items: items, align: 'start', placement: 'top',
                keepFocus: !!(input && document.activeElement === input)
            });
        });
    }

    var sendMsgBtn = document.getElementById('lxmf-send-message-btn');
    if (sendMsgBtn) {
        sendMsgBtn.addEventListener('click', function(e) {
            e.stopPropagation();
            openComposeMenu(sendMsgBtn);
        });
    }

    initComposeFab();

    // Mobile: Enter always inserts newline (avoid OSK return-key accidental sends).
    // Desktop: Enter sends, Shift+Enter inserts newline.
    var textarea = document.getElementById('lxmf-input');
    if (textarea) {
        RS.composer.bindTypingPolicy(textarea);
        textarea.removeAttribute('maxlength');
        textarea.addEventListener('keydown', function(e) {
            var wrap = _composerFormatMarker(e);
            if (wrap) {
                e.preventDefault();
                RS.markdown.toggleWrap(this, wrap);
                return;
            }
            if (RS.composer.shouldSendOnEnter(e)) {
                e.preventDefault();
                sendLxmfMessage('auto');
            }
        });
        textarea.addEventListener('input', function() {
            var counter = document.getElementById('lxmf-char-count');
            if (counter) {
                var byteLen = _utf8ByteLength(this.value);
                var efficientBytes = lxmfLimits.efficient_resource_bytes || 1048575;
                var maxBytes = lxmfLimits.max_message_bytes || 134217727;
                counter.hidden = byteLen <= efficientBytes;
                if (byteLen > efficientBytes) {
                    counter.textContent = prettySize(byteLen) + ' / ' + prettySize(maxBytes);
                    counter.classList.toggle('is-over', byteLen > maxBytes);
                }
            }
        });
    }

    var chatHeaderMenuBtn = document.getElementById('chat-header-menu-btn');
    if (chatHeaderMenuBtn) {
        chatHeaderMenuBtn.addEventListener('click', function(e) {
            e.stopPropagation();
            openChatHeaderDropdown(e.currentTarget);
        });
    }

    // The whole identity is one button for details.
    var chatWho = document.getElementById('lxmf-chat-who');
    if (chatWho) {
        chatWho.addEventListener('click', function(e) {
            e.stopPropagation();
            if (lxmfActiveContact) showContactAbout(lxmfActiveContact);
        });
    }

    var gameBtn = document.getElementById('game-challenge-btn');
    if (gameBtn) {
        gameBtn.hidden = typeof window.openNewGame !== 'function';
        gameBtn.addEventListener('click', function() {
            if (lxmfActiveContact) _startGameWith(lxmfActiveContact);
        });
    }

    if (lxmfContacts.length > 0) {
        RS.invoke('check_contact_status').catch(function() {});
    }

    var lxmfBackBtn = document.getElementById('lxmf-back-btn');
    if (lxmfBackBtn) {
        lxmfBackBtn.addEventListener('click', function(e) {
            e.stopPropagation();
            if (_clearMessageSelection()) return;
            RS.viewStack.pop();
        });
    }

    var selectDeleteBtn = document.getElementById('lxmf-select-delete-btn');
    if (selectDeleteBtn) selectDeleteBtn.addEventListener('click', _deleteSelectedMessages);
    var selectCancelBtn = document.getElementById('lxmf-select-cancel-btn');
    if (selectCancelBtn) selectCancelBtn.addEventListener('click', _clearMessageSelection);

    (function initChatSwipeBack() {
        var chatArea = document.getElementById('lxmf-chat-area');
        if (!chatArea) return;
        RS.gestures.attachSwipe(chatArea, {
            direction: 'right',
            edgeZone: RS.gestures.EDGE_ZONE_PX,
            distanceThreshold: RS.gestures.SWIPE_DISTANCE_DRILLBACK_PX,
            hapticAt: { commit: 'selection' },
            onProgress: function(dx) {
                if (dx <= 0) return;
                chatArea.style.transition = 'none';
                chatArea.style.transform = 'translateX(' + dx + 'px)';
                chatArea.style.opacity = Math.max(0.3, 1 - dx / chatArea.offsetWidth);
            },
            onCommit: function() {
                _clearMessageSelection();
                chatArea.style.transition = 'transform 0.25s ease, opacity 0.25s ease';
                chatArea.style.transform = 'translateX(100%)';
                chatArea.style.opacity = '0';
                setTimeout(function() {
                    chatArea.style.transition = '';
                    chatArea.style.transform = '';
                    chatArea.style.opacity = '';
                    RS.viewStack.pop();
                }, 250);
            },
            onCancel: function() {
                chatArea.style.transition = 'transform 0.25s ease, opacity 0.25s ease';
                chatArea.style.transform = '';
                chatArea.style.opacity = '';
            }
        });
    })();
});

RS.listen('unread_total', function(data) {
    if (typeof setMessageUnreadSource === 'function') {
        setMessageUnreadSource('direct', data && data.count);
    }
});

function showConversationDeleteDialog(hash, name) {
    rsChoice({
        title: 'Delete Conversation',
        message: 'Delete conversation with "' + name + '"?',
        choices: [
            { label: 'Remove from List', value: 'hide', hint: 'Reappears if they message again.' },
            { label: 'Delete All Messages', value: 'delete', danger: true, hint: 'Permanently removes all messages.' }
        ],
        cancelText: false
    }).then(function(choice) {
        if (!choice) return;
        if (choice === 'hide') {
            RS.invoke('hide_conversation', { hash: hash }).catch(function() {});
        } else if (choice === 'delete') {
            RS.invoke('delete_conversation', { hash: hash }).catch(function() {});
        }
    });
}

RS.listen('conversation_hidden', function(data) {
    if (!data.ok) return;
    var hash = _canonicalConversationHash(data.hash);
    _cacheDelContact(hash);
    if (_canonicalConversationHash(lxmfActiveContact) === hash) {
        _activateConversation(null, 'conversation_hidden');
        lxmfConversation = [];
        renderConversation();
    }
    if (_canonicalConversationHash(_ghostConversationHash) === hash) _removeGhostRow();
    loadConversations();
});

RS.listen('conversation_deleted', function(data) {
    if (!data.ok) return;
    var hash = _canonicalConversationHash(data.hash);
    if (typeof data.thread === 'string') {
        // One conversation went; the contact and its others stay.
        var thread = _canonicalConversationThread(data.thread);
        cacheDel(_conversationKey(hash, thread));
        if (_canonicalConversationHash(lxmfActiveContact) === hash) {
            if (thread && thread === lxmfActiveThread) {
                switchConversationThread('');
            } else if (thread === lxmfActiveThread) {
                lxmfConversation = [];
                _loadConversation(hash);
            } else {
                _refreshActiveConversationThreads();
            }
        }
        loadConversations();
        return;
    }
    _cacheDelContact(hash);
    if (_canonicalConversationHash(lxmfActiveContact) === hash) {
        _activateConversation(null, 'conversation_deleted');
        lxmfConversation = [];
        renderConversation();
    }
    if (_canonicalConversationHash(_ghostConversationHash) === hash) _removeGhostRow();
    loadConversations();
});

// 30s re-check so identity/path changes surface without a page reload.
setInterval(function() {
    if (document.hidden || !isViewActive('message')) return;
    if (lxmfContacts.length > 0) {
        RS.invoke('check_contact_status').catch(function() {});
    }
}, 30000);


function _filenameWithImageExtension(filename, mime) {
    var name = (filename || 'image').trim() || 'image';
    if (/\.[A-Za-z0-9]{1,8}$/.test(name)) return name;
    var ext = ({
        'image/jpeg': 'jpg',
        'image/jpg': 'jpg',
        'image/png': 'png',
        'image/gif': 'gif',
        'image/webp': 'webp',
        'image/heic': 'heic',
        'image/heif': 'heif',
        'image/bmp': 'bmp'
    })[(mime || '').toLowerCase()] || 'png';
    return name + '.' + ext;
}

function _fileFromDataUrl(src, filename, mime) {
    var match = /^data:([^;,]+);base64,(.*)$/i.exec(src || '');
    if (!match) return null;
    var resolvedMime = mime || match[1] || 'image/png';
    var resolvedName = _filenameWithImageExtension(filename, resolvedMime);
    var raw = atob(match[2]);
    var bytes = new Uint8Array(raw.length);
    for (var i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
    var blob = new Blob([bytes], { type: resolvedMime });
    return {
        url: URL.createObjectURL(blob),
        blob: blob,
        size: blob.size,
        filename: resolvedName,
        mime: resolvedMime,
        data_base64: match[2]
    };
}

function _fileFromImageElement(img) {
    if (!img) return Promise.reject(new Error('No image selected'));
    var stored = img.getAttribute('data-stored-name') ||
        (img.closest('.msg-image-btn') && img.closest('.msg-image-btn').getAttribute('data-stored-name'));
    if (stored) {
        var cached = _getImageDownloadFile(stored);
        if (cached) return Promise.resolve(cached);
        return RS.fileDownload(stored).then(function(file) {
            _rememberImageBlobUrl(stored, file.url, file);
            return file;
        });
    }
    var filename = img.getAttribute('data-filename') || 'image';
    var mime = img.getAttribute('data-mime') || '';
    var file = _fileFromDataUrl(img.src, filename, mime);
    if (file) return Promise.resolve(file);
    return Promise.reject(new Error('Image data is not ready yet'));
}

function _blobFromDownloadedFile(file) {
    if (file && file.blob) return file.blob;
    var raw = atob(file.data_base64 || '');
    var bytes = new Uint8Array(raw.length);
    for (var i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
    return new Blob([bytes], { type: file.mime || 'image/png' });
}

function _canCopyDownloadedImages() {
    if (typeof isAndroid === 'function' && isAndroid()) return false;
    return !!(navigator.clipboard && typeof navigator.clipboard.write === 'function' && typeof ClipboardItem !== 'undefined');
}

function _copyDownloadedImage(file) {
    if (!_canCopyDownloadedImages()) {
        return Promise.reject(new Error('Image copy is not supported on this device'));
    }
    var blob = _blobFromDownloadedFile(file);
    var mime = /^image\//i.test(file.mime || '') ? file.mime : 'image/png';
    return navigator.clipboard.write([new ClipboardItem({ [mime]: blob })]);
}

function _syncImageViewerActions(viewer) {
    var copyBtn = viewer && viewer.querySelector ? viewer.querySelector('#image-viewer-copy') : null;
    if (!copyBtn) return;
    var canCopy = _canCopyDownloadedImages();
    copyBtn.hidden = !canCopy;
    copyBtn.style.display = canCopy ? '' : 'none';
    copyBtn.setAttribute('aria-hidden', canCopy ? 'false' : 'true');
}

function _ensureImageViewer() {
    var existing = document.getElementById('rs-image-viewer');
    if (existing) {
        _syncImageViewerActions(existing);
        return existing;
    }
    var viewer = document.createElement('div');
    viewer.id = 'rs-image-viewer';
    viewer.className = 'image-viewer lightbox-zoomable';
    viewer.setAttribute('hidden', '');
    viewer.innerHTML =
        '<div class="image-viewer-backdrop" data-image-viewer-close></div>' +
        '<div class="image-viewer-toolbar">' +
            '<button type="button" class="ui-icon-btn ui-icon-btn--media" id="image-viewer-copy" title="Copy image" aria-label="Copy image">' + RS.kit.icon('copy') + '</button>' +
            '<button type="button" class="ui-icon-btn ui-icon-btn--media" id="image-viewer-save" title="Save image" aria-label="Save image">' + RS.kit.icon('download') + '</button>' +
            '<button type="button" class="ui-icon-btn ui-icon-btn--media" id="image-viewer-close" title="Close" aria-label="Close">' + RS.kit.icon('x') + '</button>' +
        '</div>' +
        '<div class="image-viewer-stage">' +
            '<img id="image-viewer-img" class="image-viewer-img" alt="">' +
        '</div>';
    document.body.appendChild(viewer);

    function close() { closeImageViewer(); }
    var stage = viewer.querySelector('.image-viewer-stage');
    var viewerImg = viewer.querySelector('#image-viewer-img');
    viewer.querySelector('[data-image-viewer-close]').addEventListener('click', close);
    viewer.querySelector('#image-viewer-close').addEventListener('click', close);
    stage.addEventListener('click', function(e) {
        if (e.target === stage) closeImageViewer();
    });
    viewerImg.addEventListener('click', function(e) {
        e.stopPropagation();
        if (viewer._suppressImageClickUntil && Date.now() < viewer._suppressImageClickUntil) return;
        var zoomed = this.classList.toggle('is-zoomed');
        viewer.classList.toggle('is-zoomed', zoomed);
        if (!this.classList.contains('is-zoomed')) {
            var stage = viewer.querySelector('.image-viewer-stage');
            if (stage) { stage.scrollTop = 0; stage.scrollLeft = 0; }
        }
    });
    viewer.querySelector('#image-viewer-copy').addEventListener('click', function(e) {
        e.stopPropagation();
        var source = viewer._sourceImage;
        _fileFromImageElement(source).then(_copyDownloadedImage).then(function() {
            showCopyConfirmationToast('Image');
        }).catch(function(err) {
            showToast((err && err.message) || 'Could not copy image', 'toast-error', 3000);
        });
    });
    viewer.querySelector('#image-viewer-save').addEventListener('click', function(e) {
        e.stopPropagation();
        var source = viewer._sourceImage;
        var stored = source && (source.getAttribute('data-stored-name') ||
            (source.closest('.msg-image-btn') && source.closest('.msg-image-btn').getAttribute('data-stored-name')));
        if (stored) {
            RS.saveFile(stored, { preferPhotos: true }).then(function(saved) {
                if (saved !== false) showToast('Saved', 'toast-success', 2500);
            }).catch(function(err) {
                showToast('Could not save: ' + ((err && err.message) || 'unknown error'), 'toast-error', 4000);
            });
            return;
        }
        _fileFromImageElement(source).then(function(file) {
            return _saveDownloadedMediaFile(file, { preferPhotos: true });
        }).catch(function(err) {
            showToast('Could not save: ' + ((err && err.message) || 'unknown error'), 'toast-error', 4000);
        });
    });
    document.addEventListener('keydown', function(e) {
        if (e.key === 'Escape' && viewer.classList.contains('open')) closeImageViewer();
    });
    _wireImageViewerSwipeDismiss(viewer, stage, viewerImg);
    _syncImageViewerActions(viewer);
    return viewer;
}

function _wireImageViewerSwipeDismiss(viewer, stage, img) {
    if (!viewer || !stage || !img) return;
    var swipe = null;

    function mobileSwipeEnabled() {
        if (typeof isTauriDesktop === 'function' && isTauriDesktop()) return false;
        return typeof isMobile === 'function' && isMobile();
    }

    function resetSwipe(animated) {
        if (animated) img.classList.add('is-settling');
        img.classList.remove('is-dragging');
        img.style.transform = '';
        img.style.opacity = '';
        if (animated) {
            setTimeout(function() { img.classList.remove('is-settling'); }, 180);
        } else {
            img.classList.remove('is-settling');
        }
        swipe = null;
    }

    stage.addEventListener('pointerdown', function(e) {
        if (!mobileSwipeEnabled()) return;
        if (e.pointerType === 'mouse') return;
        if (!viewer.classList.contains('open')) return;
        if (img.classList.contains('is-zoomed')) return;
        if (e.target !== img) return;
        swipe = {
            id: e.pointerId,
            x: e.clientX,
            y: e.clientY,
            lastX: e.clientX,
            lastY: e.clientY,
            lastAt: Date.now(),
            dragging: false
        };
        try { stage.setPointerCapture(e.pointerId); } catch (_) {}
    });

    stage.addEventListener('pointermove', function(e) {
        if (!swipe || e.pointerId !== swipe.id) return;
        var dx = e.clientX - swipe.x;
        var dy = e.clientY - swipe.y;
        var ax = Math.abs(dx);
        var ay = Math.abs(dy);
        var now = Date.now();
        var dt = Math.max(1, now - swipe.lastAt);
        swipe.velocityY = (e.clientY - swipe.lastY) / dt;
        swipe.lastX = e.clientX;
        swipe.lastY = e.clientY;
        swipe.lastAt = now;
        if (!swipe.dragging && ay > 12 && ay > ax * 1.15) {
            swipe.dragging = true;
            img.classList.add('is-dragging');
        }
        if (!swipe.dragging) return;
        e.preventDefault();
        var scale = Math.max(0.82, 1 - Math.min(ay, 260) / 1300);
        var opacity = Math.max(0.35, 1 - Math.min(ay, 260) / 320);
        img.style.transform = 'translate3d(' + Math.round(dx * 0.18) + 'px,' + Math.round(dy) + 'px,0) scale(' + scale.toFixed(3) + ')';
        img.style.opacity = opacity.toFixed(3);
    });

    function finishSwipe(e) {
        if (!swipe || e.pointerId !== swipe.id) return;
        var dy = (typeof e.clientY === 'number' ? e.clientY : swipe.lastY) - swipe.y;
        var velocity = Math.abs(swipe.velocityY || 0);
        var shouldClose = swipe.dragging && (Math.abs(dy) > 64 || (Math.abs(dy) > 36 && velocity > 0.45));
        if (swipe.dragging) viewer._suppressImageClickUntil = Date.now() + 300;
        try { stage.releasePointerCapture(swipe.id); } catch (_) {}
        if (shouldClose) {
            img.classList.remove('is-dragging');
            closeImageViewer();
        } else {
            resetSwipe(!!swipe.dragging);
        }
    }

    stage.addEventListener('pointerup', finishSwipe);
    stage.addEventListener('pointercancel', function(e) {
        if (!swipe || e.pointerId !== swipe.id) return;
        var shouldClose = swipe.dragging && Math.abs((swipe.lastY || swipe.y) - swipe.y) > 48;
        try { stage.releasePointerCapture(swipe.id); } catch (_) {}
        if (shouldClose) {
            img.classList.remove('is-dragging');
            closeImageViewer();
        } else {
            resetSwipe(!!swipe.dragging);
        }
    });
}

// The app UI never pinch-zooms; only the expanded image viewer may. Restoring
// maximum-scale=1 on close also snaps any viewer zoom back to 1.
function _setPageZoomAllowed(allowed) {
    document.documentElement.classList.toggle('page-zoom-allowed', !!allowed);
    var meta = document.querySelector('meta[name="viewport"]');
    if (!meta) return;
    var content = meta.getAttribute('content') || '';
    var zoom = allowed ? 'maximum-scale=5.0, user-scalable=yes' : 'maximum-scale=1.0, user-scalable=no';
    meta.setAttribute('content', content.replace(/maximum-scale=[^,]*,\s*user-scalable=[^,]*/, zoom));
}

function openImageViewer(img) {
    if (!img || !img.src) return;
    var viewer = _ensureImageViewer();
    _syncImageViewerActions(viewer);
    var viewerImg = viewer.querySelector('#image-viewer-img');
    var stage = viewer.querySelector('.image-viewer-stage');
    viewer._sourceImage = img;
    viewerImg.src = img.src;
    viewerImg.alt = img.getAttribute('data-filename') || 'Image';
    viewer.classList.remove('is-zoomed');
    viewerImg.classList.remove('is-zoomed');
    viewerImg.classList.remove('is-dragging', 'is-settling');
    viewerImg.style.transform = '';
    viewerImg.style.opacity = '';
    if (stage) { stage.scrollTop = 0; stage.scrollLeft = 0; }
    viewer.removeAttribute('hidden');
    // Force a layout flush so open animation starts from hidden state.
    // eslint-disable-next-line no-unused-expressions
    viewer.offsetHeight;
    viewer.classList.add('open');
    document.body.classList.add('image-viewer-open');
    _setPageZoomAllowed(true);
}

function closeImageViewer() {
    var viewer = document.getElementById('rs-image-viewer');
    if (!viewer || !viewer.classList.contains('open')) return false;
    viewer.classList.remove('open');
    viewer.classList.remove('is-zoomed');
    document.body.classList.remove('image-viewer-open');
    _setPageZoomAllowed(false);
    setTimeout(function() {
        if (!viewer.classList.contains('open')) {
            viewer.setAttribute('hidden', '');
            var img = viewer.querySelector('#image-viewer-img');
            if (img) {
                img.removeAttribute('src');
                img.classList.remove('is-dragging', 'is-settling', 'is-zoomed');
                img.style.transform = '';
                img.style.opacity = '';
            }
            viewer._sourceImage = null;
        }
    }, 160);
    return true;
}

window.RS = window.RS || {};
window.RS.closeImageViewer = closeImageViewer;
