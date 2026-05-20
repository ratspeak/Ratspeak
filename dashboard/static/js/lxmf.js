var lxmfIdentity = null;
var lxmfContacts = [];
var lxmfConversations = [];
var lxmfActiveContact = null;
var lxmfConversation = [];
var lxmfPendingFile = null;
var contactIdentityStatus = {};
var _ghostConversationHash = null;
var _replyTarget = null;
var _msgReactions = {};
var _lxmfDrafts = {};
var _conversationCache = {};
var _cacheLru = [];
var _cacheMax = 30;
var _imageBlobUrlCache = {};
var _imageBlobUrlLru = [];
var _imageBlobUrlMax = 64;
var _lxmfSendInputWasFocused = null;
var _lxmfSendInputFocusCapturedAt = 0;
var _lxmfMessageScrollTop = 0;
var _lxmfLastUserScrollUpAt = 0;
var _lxmfProgrammaticScrollUntil = 0;
var _lxmfScrollSettleToken = 0;
var _messageLongPressDetachFns = [];
var _pendingAttachmentToken = 0;
var _pendingLxmfCancelByClientId = {};
var lxmfLimits = {
    max_attachment_bytes: 134217727,
    max_message_bytes: 134217727,
    efficient_resource_bytes: 1048575,
    default_propagation_limit_kb: 256,
    propagation_transfer_limit_kb: null,
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

function _voiceStatusLabel(status) {
    switch (status) {
        case 'calling': return 'Calling';
        case 'available': return 'Calling';
        case 'ringing': return 'Ringing';
        case 'connecting': return 'Connecting';
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
    if (_voiceIncomingMatchesContact(hash)) return 'Answer call';
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
    if (_voiceIncomingMatchesContact(hash)) return _voiceAnswerCall();
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
        danger: activeMatches,
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
    try { bridge.startCallAudioRoute(route); } catch (_) {}
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
    try { bridge.startCallAudioRoute(route); } catch (_) {}
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
    surface.classList.toggle('is-incoming', !!incoming && !active);
    surface.classList.toggle('is-active', !!(active && active.status === 'established'));
    surface.classList.toggle('is-connecting', !!(active && active.status !== 'established'));
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
            : 'Incoming call';
        var audioIssue = active && !ids.global ? _voiceAudioIssueLabel() : '';
        if (audioIssue) status += ' - ' + audioIssue;
        statusEl.textContent = status;
    }

    var showIncomingActions = !!incoming && !active;
    if (answerBtn) answerBtn.style.display = showIncomingActions ? '' : 'none';
    if (rejectBtn) rejectBtn.style.display = showIncomingActions ? '' : 'none';
    if (hangupBtn) {
        hangupBtn.style.display = active ? '' : 'none';
        hangupBtn.innerHTML = _voiceIcon('phone', 16) + '<span>Hang up</span>';
    }

    var showCallControls = !!(active && active.status === 'established');
    if (controls) controls.hidden = !showCallControls;
    if (muteBtn) {
        var muteLabel = lxstVoiceState.microphoneMuted ? 'Unmute microphone' : 'Mute microphone';
        if (!lxstVoiceState.audioMicrophone) muteLabel = 'Microphone unavailable';
        muteBtn.style.display = showCallControls ? '' : 'none';
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
        showToast(message, className || 'toast-orange', 2500);
    }
}

function _voiceRestoreHeaderStatus() {
    var statusEl = document.getElementById('lxmf-chat-header-status');
    if (!statusEl || !lxmfActiveContact) return;
    var peer = (typeof _peerInfo === 'function') ? _peerInfo(lxmfActiveContact) : null;
    var statusText = (typeof _peerHeaderStatus === 'function') ? _peerHeaderStatus(peer) : '';
    var statusOnline = !!(peer && peer.route_state && peer.route_state !== 'none');
    statusEl.textContent = statusText;
    statusEl.style.display = statusText ? '' : 'none';
    statusEl.className = 'lxmf-chat-header-status' + (statusOnline ? ' is-online' : '');
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

function _voiceStartCall(hash) {
    if (!lxstVoiceState.available || !hash) return Promise.resolve();
    var dialToken = ++_voiceDialToken;
    _voiceHaptic('selection');
    _voiceBlockMobileNavigation(280);
    _voiceResetCallControls();
    lxstVoiceState.lastDialHash = hash;
    _voiceSetOptimisticOutgoing(hash);
    _voicePrimeNativeCallRoute();
    renderVoiceUi();
    return _voiceAfterNextPaint().then(_voiceEnsurePlaybackReady).then(_voiceEnsureMicrophonePermission).then(function() {
        if (dialToken !== _voiceDialToken || !_voiceActiveMatchesContact(hash)) return;
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
    _voiceStopRingtone();
    _voiceHaptic('selection');
    return _voiceEnsurePlaybackReady().then(_voiceEnsureMicrophonePermission).then(function() {
        _voiceResetCallControls();
        _voicePrimeNativeCallRoute();
        return RS.invoke('voice_answer').then(function() {
            _voiceHaptic('success');
            lxstVoiceState.incoming = null;
            renderVoiceUi();
        }).catch(function(err) {
            _voiceReleaseNativeCallRoutePrime();
            _voiceHaptic('error');
            _voiceNotify((err && err.message) || 'Could not answer call');
        });
    });
}

function _voiceRejectCall() {
    _voiceStopRingtone();
    _voiceCancelPendingDial();
    _voiceHaptic('warning');
    _voiceSuppressNoAnswerCueUntil = Date.now() + 2000;
    return RS.invoke('voice_reject').catch(function() {}).then(function() {
        lxstVoiceState.incoming = null;
        renderVoiceUi();
    });
}

function _voiceHangupCall() {
    _voiceStopRingtone();
    _voiceCancelPendingDial();
    _voiceHaptic('warning');
    _voiceSuppressNoAnswerCueUntil = Date.now() + 2000;
    return RS.invoke('voice_hangup').catch(function(err) {
        _voiceHaptic('error');
        _voiceNotify((err && err.message) || 'Could not hang up call');
    }).then(function() {
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
        renderVoiceUi();
    });
}

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
    var callBtn = document.getElementById('lxst-call-btn');
    var active = lxstVoiceState.active;
    var incoming = lxstVoiceState.incoming;
    var activeMatches = _voiceActiveMatchesContact();
    var incomingMatches = _voiceIncomingMatchesContact();

    if (callBtn) {
        var canShow = lxstVoiceState.available && !!lxmfActiveContact && !activeMatches && !incomingMatches;
        var isConnecting = !!(activeMatches && active && active.status !== 'established');
        callBtn.style.display = canShow ? '' : 'none';
        callBtn.classList.toggle('is-active', !!activeMatches);
        callBtn.classList.toggle('is-connecting', isConnecting);
        callBtn.classList.toggle('is-hangup', !!activeMatches && active && active.status === 'established');
        callBtn.disabled = !!active && !activeMatches;
        callBtn.title = callBtn.disabled ? 'Call in progress' : _voicePrimaryActionLabel();
        callBtn.setAttribute('aria-label', callBtn.title);
        callBtn.innerHTML = callBtn.disabled ? _voiceIcon('phone', 18) : _voicePrimaryActionIcon();
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
    if (!incoming) {
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
        lxstVoiceState.incoming = {
            link_id: data.link_id,
            remote_identity: data.remote_identity,
            remote_lxmf_destination: data.remote_lxmf_destination || null,
            status: 'ringing'
        };
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
    hash = String(hash).trim();
    if (!hash) return null;
    var services = Array.isArray(c.services) ? c.services.slice() : [];
    return {
        hash: hash,
        display_name: c.display_name || '',
        trust: c.trust || '',
        notes: c.notes || '',
        first_seen: c.first_seen,
        last_seen: c.last_seen,
        services: services,
        supports_ratspeak: services.indexOf('ratspeak.client') !== -1
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

function _lxmfMessageBottomGap(container) {
    if (!container) return 0;
    return Math.max(0, container.scrollHeight - container.clientHeight - container.scrollTop);
}

function _lxmfMessagesNearBottom(container) {
    if (!container) return true;
    return _lxmfMessageBottomGap(container) <= 160;
}

function _wireLxmfMessageScroll(container) {
    if (!container || container._lxmfScrollAttached) return;
    container._lxmfScrollAttached = true;
    _lxmfMessageScrollTop = container.scrollTop;
    container.addEventListener('scroll', function() {
        var now = Date.now();
        var currentTop = container.scrollTop;
        if (now >= _lxmfProgrammaticScrollUntil && currentTop < _lxmfMessageScrollTop - 2) {
            _lxmfLastUserScrollUpAt = now;
        }
        if (_lxmfMessagesNearBottom(container)) {
            _lxmfLastUserScrollUpAt = 0;
        }
        _lxmfMessageScrollTop = currentTop;
    }, { passive: true });
    container.addEventListener('wheel', function(e) {
        if (e.deltaY < -1) _lxmfLastUserScrollUpAt = Date.now();
    }, { passive: true });
}

function _setLxmfMessageScrollTop(container, top) {
    if (!container) return;
    _lxmfProgrammaticScrollUntil = Date.now() + 150;
    container.scrollTop = top;
    _lxmfMessageScrollTop = container.scrollTop;
}

function _scheduleLxmfScrollToBottom(container) {
    if (!container) return;
    var token = ++_lxmfScrollSettleToken;
    function pin() {
        if (token !== _lxmfScrollSettleToken || !container.isConnected) return;
        _setLxmfMessageScrollTop(container, container.scrollHeight);
    }
    pin();
    if (typeof requestAnimationFrame === 'function') requestAnimationFrame(pin);
    [40, 140, 360].forEach(function(delay) {
        setTimeout(pin, delay);
    });
}

function _compensateImageLoadScroll(container, img, before) {
    if (!container || !img || !before) return;
    if (!container.isConnected) return;
    var afterHeight = container.scrollHeight;
    var delta = afterHeight - before.scrollHeight;
    if (Math.abs(delta) < 1) return;
    var containerRect = container.getBoundingClientRect();
    // If an older image above the viewport gains height, preserve the user's
    // current reading position by moving the scroll offset by the exact delta.
    if (before.imgTop < containerRect.top + 8) {
        _setLxmfMessageScrollTop(container, container.scrollTop + delta);
    }
}

function _captureLxmfMessageScrollState(container) {
    return {
        scrollTop: container ? container.scrollTop : 0,
        scrollHeight: container ? container.scrollHeight : 0,
        nearBottom: _lxmfMessagesNearBottom(container),
    };
}

function _restoreLxmfMessageScroll(container, state) {
    if (!container || !state) return;
    _lxmfScrollSettleToken++;
    var maxScroll = Math.max(0, container.scrollHeight - container.clientHeight);
    _setLxmfMessageScrollTop(container, Math.min(maxScroll, Math.max(0, state.scrollTop)));
}

function _userIsActivelyScrollingMessagesUp() {
    return _lxmfLastUserScrollUpAt && Date.now() - _lxmfLastUserScrollUpAt < 900;
}

function _applyLxmfMessageScrollAfterRender(container, state, options) {
    options = options || {};
    var shouldPin = !!options.forceScrollBottom ||
        (!!options.stickToBottom && !_userIsActivelyScrollingMessagesUp()) ||
        (!options.preserveScroll && state && state.nearBottom && !_userIsActivelyScrollingMessagesUp());

    if (shouldPin) {
        _scheduleLxmfScrollToBottom(container);
    } else {
        _restoreLxmfMessageScroll(container, state);
    }
    return shouldPin;
}

function _watchLxmfImagesForBottomPin(container, shouldPin) {
    if (!container) return;
    container.querySelectorAll('img').forEach(function(img) {
        if (img.complete && img.naturalWidth) return;
        var before = {
            scrollHeight: container.scrollHeight,
            imgTop: img.getBoundingClientRect().top
        };
        img.addEventListener('load', function() {
            if (shouldPin && !_userIsActivelyScrollingMessagesUp()) {
                _scheduleLxmfScrollToBottom(container);
                return;
            }
            if (typeof requestAnimationFrame === 'function') {
                requestAnimationFrame(function() {
                    _compensateImageLoadScroll(container, img, before);
                });
            } else {
                _compensateImageLoadScroll(container, img, before);
            }
        }, { once: true });
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

function _peerRouteLabel(peer) {
    if (!peer) return 'No current path';
    if (peer.route_label) return peer.route_label;
    if (peer.hops !== null && peer.hops !== undefined) {
        return peer.hops === 0 ? 'Direct' : peer.hops + ' hop' + (peer.hops !== 1 ? 's' : '');
    }
    return peer.in_path ? 'Available' : 'No current path';
}

function _peerActivityLabel(peer) {
    if (!peer) return 'Never seen';
    return peer.activity_label || 'Never seen';
}

function _peerHeaderStatus(peer) {
    if (!peer) return '';
    if (peer.in_path || (peer.route_state && peer.route_state !== 'none')) return 'Reachable';
    if (typeof formatLastHeard === 'function') return formatLastHeard(peer.last_seen);
    return peer.last_seen ? prettyTime((Date.now() / 1000) - peer.last_seen) + ' ago' : '';
}

function _peerLastHeardLabel(peer) {
    if (typeof formatLastHeard === 'function') return formatLastHeard(peer ? peer.last_seen : null);
    if (peer && peer.last_seen) return prettyTime((Date.now() / 1000) - peer.last_seen) + ' ago';
    return 'No activity yet';
}

function _peerFirstHeardLabel(peer) {
    if (!peer || !peer.first_seen) return '\u2014';
    if (typeof formatLastHeard === 'function') return formatLastHeard(peer.first_seen);
    return prettyTime((Date.now() / 1000) - peer.first_seen) + ' ago';
}

function _peerPathAgeLabel(peer) {
    if (!peer || !peer.in_path || peer.path_age === null || peer.path_age === undefined) return '\u2014';
    return prettyTime(peer.path_age) + ' ago';
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
    document.querySelectorAll('.conv-row[data-hash]').forEach(function(row) {
        var hash = row.dataset.hash;
        var nameEl = row.querySelector('.conv-name');
        if (!hash || !nameEl) return;
        var payload = _conversationPayloadForHash(hash);
        var info = _conversationNameInfo(
            hash,
            payload ? payload.display_name : null,
            payload ? payload.is_contact : false
        );
        nameEl.innerHTML = ratspeakDisplayNameHtml(info.name, hash);
        nameEl.classList.toggle('is-hash', !!info.isHash);
        nameEl.title = info.isHash ? hash : '';
    });

    if (lxmfActiveContact) {
        var activeInfo = _conversationNameInfo(lxmfActiveContact, null, false);
        var headerName = document.getElementById('lxmf-chat-header-name');
        if (headerName) headerName.innerHTML = ratspeakDisplayNameHtml(activeInfo.name, lxmfActiveContact);
        var emptyName = document.querySelector('.lxmf-empty-conv-name');
        if (emptyName) emptyName.innerHTML = ratspeakDisplayNameHtml(activeInfo.name, lxmfActiveContact);
    }
}

function _peerViaLabel(peer) {
    if (!peer || !peer.in_path) return '\u2014';
    if (typeof formatVia === 'function') return formatVia(peer.via);
    return peer.via || 'direct';
}

function _peerInterfaceLabel(peer) {
    if (!peer || !peer.iface) return '\u2014';
    return peer.iface + (peer.iface_is_live ? '' : ' (last known)');
}

function _deliveryPrefOrAuto(method) {
    return method || 'auto';
}

function _optimisticDeliveryMethod(method) {
    return method === 'auto' ? null : method;
}

function _utf8ByteLength(value) {
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

function linkifyMessageText(text) {
    var value = String(text || '');
    if (!value) return '';
    var out = '';
    var last = 0;
    value.replace(RS_LINK_RE, function(match, schemeUrl, bareUrl, offset) {
        var prev = offset > 0 ? value.charAt(offset - 1) : '';
        if (prev === '@') return match;
        out += escapeHtml(value.slice(last, offset));
        var split = _splitTrailingUrlPunctuation(match);
        var label = split.url;
        var href = /^https?:\/\//i.test(label) ? label : 'https://' + label;
        out += '<a class="rs-message-link" href="' + escapeHtml(href) +
            '" data-url="' + escapeHtml(href) + '" rel="noopener noreferrer">' +
            escapeHtml(label) + '</a>' + escapeHtml(split.trailing);
        last = offset + match.length;
        return match;
    });
    out += escapeHtml(value.slice(last));
    return out.replace(/\n/g, '<br>');
}

// Icons must distinguish proof-backed delivery from accepted-for-forwarding.
// Opportunistic sends have no LXMF receipt; propagation means node deposit,
// not end-to-end recipient delivery.
function _messageStateIconHtml(msg) {
    var state = msg.state;
    var method = msg.delivery_method || 'opportunistic';
    var SVG_OPEN = '<svg viewBox="0 0 16 16" fill="none" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">';
    var ICON = {
        read: '<polyline points="1 8 4 11 9 4"/><polyline points="5 8 8 11 13 4"/>',
        check: '<polyline points="3 8 7 12 13 4"/>',
        clock: '<circle cx="8" cy="8" r="6"/><polyline points="8 4 8 8 11 10"/>',
        x: '<line x1="4" y1="4" x2="12" y2="12"/><line x1="12" y1="4" x2="4" y2="12"/>',
        rejected: '<circle cx="8" cy="8" r="6"/><line x1="5" y1="5" x2="11" y2="11"/><line x1="11" y1="5" x2="5" y2="11"/>',
        envelope: '<rect x="2" y="4" width="12" height="9" rx="1.5"/><polyline points="2.5 5 8 10 13.5 5"/>'
    };
    function wrap(cls, label, body) {
        return ' <span class="msg-state-icon ' + cls + '" role="img" aria-label="' + label + '">' + SVG_OPEN + body + '</svg></span>';
    }

    if (state === 'read') return wrap('msg-state-read', 'Read', ICON.read);
    if (state === 'failed' || state === 'timeout') return wrap('msg-state-failed', 'Failed', ICON.x);
    if (state === 'cancelled') return wrap('msg-state-cancelled', 'Cancelled', ICON.x);
    if (state === 'rejected') return wrap('msg-state-rejected', 'Rejected', ICON.rejected) + ' <span class="msg-state-label">Rejected</span>';
    if (state === 'propagated') return wrap('msg-state-propagated', 'Stored in Offline Inbox', ICON.envelope);
    if (state === 'delivered' && method === 'direct') return wrap('msg-state-delivered-direct', 'Delivered', ICON.check);
    if ((state === 'sent' || state === 'delivered') && method !== 'direct') return wrap('msg-state-sent', 'Sent', ICON.check);
    // In-flight: sending, routing, propagating, generating, outbound,
    // resending, or `sent` awaiting a Direct LXMF link receipt.
    var progressPercent = _messageProgressPercent(msg);
    var label = progressPercent !== null
        ? 'Sending ' + progressPercent + '%'
        : 'Sending';
    return wrap('msg-state-sending', label, ICON.clock);
}

function _isCanonicalLxmfMsgId(msgId) {
    return /^[a-f0-9]{64}$/i.test(String(msgId || ''));
}

function _isTerminalMessageState(state) {
    return ['delivered', 'propagated', 'failed', 'cancelled', 'rejected', 'timeout'].indexOf(state) !== -1;
}

function _messageCanCancelSend(msg) {
    if (!msg || msg.direction !== 'outbound' || _isTerminalMessageState(msg.state)) return false;
    return [
        'outbound',
        'generating',
        'sending',
        'routing',
        'resolving',
        'propagating',
        'resending',
        'link_establishing',
        'sending_via_link',
        'reusing_direct_link',
        'reusing_backchannel'
    ].indexOf(msg.state) !== -1;
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
    return Math.max(1, Math.min(99, Math.round(progress * 100)));
}

function _messageProgressMetaHtml(msg) {
    var percent = _messageProgressPercent(msg);
    return percent === null ? '' : '<span class="msg-send-progress">' + percent + '%</span>';
}

function _messageSendCancelOverlayHtml(msg, percent) {
    var pct = percent === null ? 0 : percent;
    return '<button type="button" class="lxmf-send-cancel" ' +
        'data-msg-id="' + escapeHtml(msg.id || '') + '" ' +
        'style="--send-progress:' + pct + '%" aria-label="Cancel send">' +
        '<span aria-hidden="true">&times;</span>' +
    '</button>';
}

function _messageInlineCancelHtml(msg) {
    if (!_messageCanCancelTransfer(msg)) return '';
    return '<button type="button" class="msg-send-cancel-inline" ' +
        'data-msg-id="' + escapeHtml(msg.id || '') + '" aria-label="Cancel send">&times;</button>';
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
    renderConversation();
}

function _invokeLxmfCancel(msgId) {
    return RS.invoke('cancel_lxmf_message', {
        args: { msg_id: msgId }
    });
}

function _flushPendingLxmfCancel(clientMsgId, serverMsgId) {
    if (!clientMsgId || !serverMsgId || !_pendingLxmfCancelByClientId[clientMsgId]) return;
    delete _pendingLxmfCancelByClientId[clientMsgId];
    _invokeLxmfCancel(serverMsgId).catch(function(err) {
        showToast('Cancel failed: ' + ((err && err.message) || 'error'), 'toast-red', 3500);
    });
}

function _cancelLxmfSend(msgId) {
    msgId = String(msgId || '');
    if (!msgId) return;
    _markLxmfMessageCancelled(msgId);
    if (!_isCanonicalLxmfMsgId(msgId)) {
        _pendingLxmfCancelByClientId[msgId] = true;
        return;
    }
    _invokeLxmfCancel(msgId).catch(function(err) {
        showToast('Cancel failed: ' + ((err && err.message) || 'error'), 'toast-red', 3500);
    });
}

function _handleLxmfSendAccepted(resp, clientMsgId) {
    var serverMsgId = resp && resp.msg_id;
    if (!serverMsgId || !clientMsgId) return;
    for (var i = 0; i < lxmfConversation.length; i++) {
        if (lxmfConversation[i].id === clientMsgId) {
            lxmfConversation[i].id = serverMsgId;
            break;
        }
    }
    _flushPendingLxmfCancel(clientMsgId, serverMsgId);
}

function cacheGet(hash) {
    var msgs = _conversationCache[hash];
    if (msgs) {
        var idx = _cacheLru.indexOf(hash);
        if (idx !== -1) { _cacheLru.splice(idx, 1); _cacheLru.push(hash); }
    }
    return msgs;
}
function cacheSet(hash, messages) {
    var idx = _cacheLru.indexOf(hash);
    if (idx !== -1) _cacheLru.splice(idx, 1);
    _cacheLru.push(hash);
    _conversationCache[hash] = messages;
    while (_cacheLru.length > _cacheMax) {
        var evict = _cacheLru.shift();
        delete _conversationCache[evict];
    }
}
function cacheDel(hash) {
    delete _conversationCache[hash];
    var idx = _cacheLru.indexOf(hash);
    if (idx !== -1) _cacheLru.splice(idx, 1);
}

function _rememberImageBlobUrl(name, url, file) {
    var existing = _imageBlobUrlCache[name];
    if (existing && existing.url !== url) {
        try { URL.revokeObjectURL(existing.url); } catch (_) {}
    }
    var idx = _imageBlobUrlLru.indexOf(name);
    if (idx !== -1) _imageBlobUrlLru.splice(idx, 1);
    _imageBlobUrlLru.push(name);
    _imageBlobUrlCache[name] = { url: url, ts: Date.now(), file: file || null };
    while (_imageBlobUrlLru.length > _imageBlobUrlMax) {
        var evict = _imageBlobUrlLru.shift();
        var entry = _imageBlobUrlCache[evict];
        delete _imageBlobUrlCache[evict];
        if (entry && entry.url) {
            try { URL.revokeObjectURL(entry.url); } catch (_) {}
        }
    }
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

function _formatDateLabel(timestamp) {
    if (!timestamp) return '';
    var d = new Date(timestamp * 1000);
    var now = new Date();
    var today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
    var msgDay = new Date(d.getFullYear(), d.getMonth(), d.getDate());
    var diffDays = Math.round((today - msgDay) / 86400000);
    if (diffDays === 0) return 'Today';
    if (diffDays === 1) return 'Yesterday';
    if (diffDays < 7) return d.toLocaleDateString(undefined, { weekday: 'long' });
    if (d.getFullYear() === now.getFullYear()) return d.toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
    return d.toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: 'numeric' });
}

function renderMsgProfileStrip() {
    var avatarEl = document.getElementById('msg-profile-avatar');
    var nameEl = document.getElementById('msg-profile-name');

    var active = null;
    if (typeof identityList !== 'undefined') {
        for (var i = 0; i < identityList.length; i++) {
            if (identityList[i].is_active) { active = identityList[i]; break; }
        }
    }
    if (active) {
        var hash = active.lxmf_hash || active.hash || '';
        var displayName = active.display_name || active.nickname || 'Me';
        if (avatarEl) avatarEl.innerHTML = identityAvatar(hash, 36);
        if (nameEl) nameEl.textContent = displayName;

        var hdrAvatar = document.getElementById('header-mobile-avatar');
        var hdrName = document.getElementById('header-mobile-name');
        var mobileName = active.display_name || active.nickname || 'Account 1';
        if (hdrAvatar) hdrAvatar.innerHTML = identityAvatar(hash, 36);
        if (hdrName) hdrName.textContent = mobileName;
    }
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
        var ghost = container.querySelector('.conv-row[data-ghost="true"]');
        if (ghost) ghost.remove();
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
    var row = container.querySelector('.conv-row[data-hash="' + hash + '"]');
    if (row) row.removeAttribute('data-ghost');
    if (_ghostConversationHash === hash) _ghostConversationHash = null;
}

function _onChatDetailExit() {
    var exitingHash = lxmfActiveContact;
    var input = document.getElementById('lxmf-input');
    if (input && exitingHash) {
        if (input.value.trim()) { _lxmfDrafts[exitingHash] = input.value; }
        else { delete _lxmfDrafts[exitingHash]; }
    }

    if (!_ghostConversationHash || _ghostConversationHash !== exitingHash) return;

    if (_conversationHasVisibleMessages()) {
        _promoteGhostConversationRow(exitingHash);
        loadConversations();
        return;
    }

    _removeGhostRow();
    cacheDel(exitingHash);
    lxmfActiveContact = null;
    lxmfConversation = [];
    if (input) {
        input.value = '';
        input.style.height = '';
    }
    renderConversation();
    _renderConversationsFromCache(lxmfConversations || []);
}

// Cache-first render to avoid an empty-spinner flash; reconciles via fetch.
function _loadConversation(hash) {
    var cached = cacheGet(hash);
    if (cached && cached.length > 0) {
        lxmfConversation = cached;
    } else {
        lxmfConversation = [];
    }
    renderConversation({ forceScrollBottom: true });
    // get_conversation fetches messages AND marks-read; broadcasts unread_total.
    RS.invoke('get_conversation', { hash: hash }).then(function(result) {
        var messages = (result && result.messages) || [];
        cacheSet(hash, messages);
        if (hash === lxmfActiveContact) {
            lxmfConversation = messages;
            renderConversation({ forceScrollBottom: true });
        }
    }).catch(function() {});
}

// Placeholder row shown until the backend confirms the conversation exists.
function _ensureGhostRow(hash) {
    var container = document.getElementById('lxmf-conversations-list');
    if (!container) return;

    if (_ghostConversationHash && _ghostConversationHash !== hash) {
        var oldGhost = container.querySelector('.conv-row[data-ghost="true"]');
        if (oldGhost) oldGhost.remove();
    }

    var existing = container.querySelector('.conv-row[data-hash="' + hash + '"]');
    if (existing) {
        container.querySelectorAll('.conv-row.active').forEach(function(r) { r.classList.remove('active'); });
        existing.classList.add('active');
        _ghostConversationHash = null;
        return;
    }

    var empty = container.querySelector('.empty-state');
    if (empty) empty.remove();

    var nameInfo = _conversationNameInfo(hash, null, false);

    var row = document.createElement('div');
    row.className = 'conv-row active';
    row.dataset.hash = hash;
    row.dataset.ghost = 'true';
    var avatarHtml = '<span class="conv-avatar">' + identityAvatar(hash, 36) + '</span>';
    row.innerHTML =
        avatarHtml +
        '<div class="conv-row-content">' +
            '<div class="conv-row-top">' +
                '<span class="conv-name' + (nameInfo.isHash ? ' is-hash' : '') + '" title="' + (nameInfo.isHash ? escapeHtml(hash) : '') + '">' + ratspeakDisplayNameHtml(nameInfo.name, hash) + '</span>' +
                '<span class="conv-time"></span>' +
            '</div>' +
            '<div class="conv-row-bottom">' +
                '<span class="conv-preview"></span>' +
            '</div>' +
        '</div>';

    container.querySelectorAll('.conv-row.active').forEach(function(r) { r.classList.remove('active'); });

    container.insertBefore(row, container.firstChild);

    row.addEventListener('click', function() {
        if (_convSwipedRecently) return;
        var clickHash = this.dataset.hash;
        lxmfActiveContact = clickHash;
        container.querySelectorAll('.conv-row.active').forEach(function(r) { r.classList.remove('active'); });
        this.classList.add('active');
        _loadConversation(clickHash);
        loadConversations();
    });

    _ghostConversationHash = hash;
}

function _updateConversationPreview(hash, previewText, timestamp) {
    var container = document.getElementById('lxmf-conversations-list');
    if (!container) return;

    var empty = container.querySelector('.empty-state');
    if (empty) empty.remove();
    var loading = container.querySelector('.loading-state');
    if (loading) loading.remove();

    var row = container.querySelector('.conv-row[data-hash="' + hash + '"]');
    if (row) {
        var previewEl = row.querySelector('.conv-preview');
        if (previewEl) previewEl.textContent = (previewText || '').substring(0, 50);
        var timeEl = row.querySelector('.conv-time');
        if (timeEl) timeEl.textContent = timestamp ? formatConvTime(timestamp) : '';
        // Drop the ghost marker so the row survives refreshes.
        row.removeAttribute('data-ghost');
        if (row !== container.firstChild) {
            container.insertBefore(row, container.firstChild);
        }
    } else {
        var nameInfo = _conversationNameInfo(hash, null, false);
        var newRow = document.createElement('div');
        newRow.className = 'conv-row active';
        newRow.dataset.hash = hash;
        var newAvatarHtml = '<span class="conv-avatar">' + identityAvatar(hash, 36) + '</span>';
        newRow.innerHTML =
            newAvatarHtml +
            '<div class="conv-row-content">' +
                '<div class="conv-row-top">' +
                    '<span class="conv-name' + (nameInfo.isHash ? ' is-hash' : '') + '" title="' + (nameInfo.isHash ? escapeHtml(hash) : '') + '">' + ratspeakDisplayNameHtml(nameInfo.name, hash) + '</span>' +
                    '<span class="conv-time">' + (timestamp ? formatConvTime(timestamp) : '') + '</span>' +
                '</div>' +
                '<div class="conv-row-bottom">' +
                    '<span class="conv-preview">' + escapeHtml((previewText || '').substring(0, 50)) + '</span>' +
                '</div>' +
            '</div>';
        container.insertBefore(newRow, container.firstChild);

        newRow.addEventListener('click', function() {
            if (_convSwipedRecently) return;
            var clickHash = this.dataset.hash;
            lxmfActiveContact = clickHash;
            container.querySelectorAll('.conv-row.active').forEach(function(r) { r.classList.remove('active'); });
            this.classList.add('active');
            _loadConversation(clickHash);
            loadConversations();
        });
    }

    if (_ghostConversationHash === hash) {
        _ghostConversationHash = null;
    }
}

function _conversationPreviewForMessage(message) {
    if (!message) return '';
    var content = (message.content || '').trim();
    if (content) return content;
    if (message.image) return 'Photo';
    if (Array.isArray(message.attachments) && message.attachments.length > 0) {
        return message.attachments[0].filename || 'Attachment';
    }
    return '';
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
        last_state: last.state || '',
        last_delivery_method: last.delivery_method || null,
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

function renderCockpitConversations() {
    renderDashboardRecentMessages();
}

function renderDashboardRecentMessages() {
    var container = document.getElementById('dashboard-recent-messages');
    if (!container) return;

    RS.invoke('api_lxmf_conversations').then(function(convos) {
        if (!convos || convos.length === 0) {
            container.innerHTML = '<div class="empty-state" style="padding:24px;">' +
                '<span class="empty-state-primary">No messages yet</span>' +
                '<span class="empty-state-hint">Send your first encrypted message</span>' +
            '</div>';
            return;
        }

        container.innerHTML = convos.slice(0, 5).map(function(c) {
            var nameInfo = _conversationNameInfo(c.hash, c.display_name, c.is_contact);
            var rawPreview = c.last_message || '';
            var dirPrefix = c.last_direction === 'outbound' ? 'You: ' : '';
            var preview = dirPrefix + rawPreview;
            var time = c.timestamp ? formatConvTime(c.timestamp) : '';
            var unreadClass = c.unread > 0 ? ' unread' : '';
            var unreadBadge = c.unread > 0 ? '<span class="conv-unread-badge">' + c.unread + '</span>' : '';
            var nameClass = 'conv-name' + (nameInfo.isHash ? ' is-hash' : '');
            var avatarHtml = '<div class="conv-avatar-wrap"><span class="conv-avatar">' + identityAvatar(c.hash, 40) + '</span></div>';

            return '<div class="conv-row' + unreadClass + '" data-hash="' + escapeHtml(c.hash) + '">' +
                avatarHtml +
                '<div class="conv-row-content">' +
                    '<div class="conv-row-top">' +
                        '<span class="' + nameClass + '" title="' + (nameInfo.isHash ? escapeHtml(c.hash) : '') + '">' + ratspeakDisplayNameHtml(nameInfo.name, c.hash) + '</span>' +
                        '<span class="conv-time">' + time + '</span>' +
                    '</div>' +
                    '<div class="conv-row-bottom">' +
                        '<span class="conv-preview">' + escapeHtml(preview) + '</span>' +
                        unreadBadge +
                    '</div>' +
                '</div>' +
            '</div>';
        }).join('');

        container.querySelectorAll('.conv-row').forEach(function(el) {
            el.addEventListener('click', function() {
                var hash = this.dataset.hash;
                lxmfActiveContact = hash;
                switchView('message');
                _loadConversation(hash);
                loadConversations();
            });
        });
    }).catch(function() {});
}

var _loadConversationsTimer = null;
var _lastConversationsLoad = 0;
var _conversationsFirstLoadDone = false;
// Stacked fetches park spawn_blocking DB connections past the frontend
// 5s timeout, which wedges the pool. Coalesce to a single in-flight call.
var _convFetchInFlight = false;

// Force path bypasses debounce + in-flight guard for error-recovery.
function loadConversationsForce() {
    _lastConversationsLoad = 0;
    _convFetchInFlight = false;
    loadConversations();
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
    if (_loadConversationsTimer) {
        clearTimeout(_loadConversationsTimer);
        _loadConversationsTimer = null;
    }
    _loadConversationsReal();
}

function _loadConversationsReal(retryCount) {
    retryCount = retryCount || 0;
    if (_convFetchInFlight && retryCount === 0) return;
    _convFetchInFlight = true;
    _lastConversationsLoad = Date.now();
    var container = document.getElementById('lxmf-conversations-list');
    if (!container) { _convFetchInFlight = false; return; }

    // Cap retries at 2 so we never park >3 spawn_blocking DB tasks at once.
    var willRetry = false;
    var scheduleRetry = function(delay) {
        willRetry = true;
        setTimeout(function() { _loadConversationsReal(retryCount + 1); }, delay);
    };

    RS.invoke('api_lxmf_conversations').then(function(convos) {
        if (!_conversationsFirstLoadDone && (!convos || convos.length === 0) && retryCount < 2) {
            scheduleRetry(1500);
            return;
        }
        _conversationsFirstLoadDone = true;
        lxmfConversations = Array.isArray(convos) ? convos : [];
        _renderConversationsFromCache(lxmfConversations);
    }).catch(function(err) {
        var transient = err && err.code === 'service_unavailable';
        if (retryCount < 2 && (transient || err)) {
            scheduleRetry(Math.min(2000, (retryCount + 1) * 1000));
            return;
        }
        // Fall back to cache; next mutation broadcast will heal us.
        if (lxmfConversations && lxmfConversations.length > 0) {
            _renderConversationsFromCache(lxmfConversations);
            return;
        }
        if (container && !container.querySelector('.conv-row')) {
            container.innerHTML = '<div class="empty-state"><svg class="empty-state-svg empty-state-svg-sm" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg><span class="empty-state-primary">Couldn\'t load conversations.</span></div>';
        }
    }).finally(function() {
        if (!willRetry) _convFetchInFlight = false;
    });
}

function _renderConversationsFromCache(convos) {
    var container = document.getElementById('lxmf-conversations-list');
    if (!container) return;

    convos = _mergeOptimisticConversation(convos);

    if (!convos || convos.length === 0) {
        if (_ghostConversationHash && _ghostConversationHash === lxmfActiveContact) {
            container.innerHTML = '';
            _ensureGhostRow(_ghostConversationHash);
        } else {
            container.innerHTML = '<div class="empty-state">' +
                '<svg class="empty-state-svg" width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/></svg>' +
                '<span class="empty-state-primary">No conversations yet</span>' +
                '<span class="empty-state-hint">Tap the compose button to start messaging</span>' +
            '</div>';
        }
        return;
    }

    var html = '';
    convos.forEach(function(c) {
            var nameInfo = _conversationNameInfo(c.hash, c.display_name, c.is_contact);
            var rawPreview = c.last_message || '';
            var dirPrefix = c.last_direction === 'outbound' ? 'You: ' : '';
            var preview = dirPrefix + rawPreview;
            var time = c.timestamp ? formatConvTime(c.timestamp) : '';
            var activeClass = (lxmfActiveContact === c.hash) ? ' active' : '';
            var unreadClass = c.unread > 0 ? ' unread' : '';
            var unreadBadge = c.unread > 0 ? '<span class="conv-unread-badge">' + c.unread + '</span>' : '';
            var nameClass = 'conv-name' + (nameInfo.isHash ? ' is-hash' : '');
            var avatarHtml = identityAvatar(c.hash, 40);

            var statusClass = 'offline';
            var _peers = _lxmfPeers();
            for (var ri = 0; ri < _peers.length; ri++) {
                if (_peers[ri].hash === c.hash) {
                    var rs = _peers[ri].status;
                    if (rs === 'reachable' || rs === 'direct') statusClass = 'online';
                    else if (rs === 'stale') statusClass = 'stale';
                    break;
                }
            }

            var previewStateHtml = '';
            if (c.last_direction === 'outbound' && c.last_state) {
                // Conversation list uses the same icon helper as the message
                // bubble. `last_delivery_method` is not in the conversations
                // payload yet, so unknown messages render as opportunistic
                // (muted ✓ for delivered/sent) by default.
                previewStateHtml = _messageStateIconHtml({
                    state: c.last_state,
                    delivery_method: c.last_delivery_method || null,
                });
            }

            html += '<div class="conv-row' + activeClass + unreadClass + '" data-hash="' + escapeHtml(c.hash) + '">' +
                '<div class="conv-avatar-wrap"><span class="conv-avatar">' + avatarHtml + '</span><span class="conv-status-dot ' + statusClass + '"></span></div>' +
                '<div class="conv-row-content">' +
                    '<div class="conv-row-top">' +
                        '<span class="' + nameClass + '" title="' + (nameInfo.isHash ? escapeHtml(c.hash) : '') + '">' + ratspeakDisplayNameHtml(nameInfo.name, c.hash) + '</span>' +
                        '<span class="conv-time">' + time + '</span>' +
                    '</div>' +
                    '<div class="conv-row-bottom">' +
                        '<span class="conv-preview">' + previewStateHtml + escapeHtml(preview) + '</span>' +
                        unreadBadge +
                    '</div>' +
                '</div>' +
            '</div>';
        });
        container.innerHTML = html;

        container.querySelectorAll('.conv-row').forEach(function(el) {
            el.addEventListener('click', function() {
                if (_convSwipedRecently) return;
                var hash = this.dataset.hash;
                if (_ghostConversationHash && _ghostConversationHash !== hash) {
                    _removeGhostRow();
                }
                // Per-thread draft persistence on switch.
                var input = document.getElementById('lxmf-input');
                if (input && lxmfActiveContact) {
                    if (input.value.trim()) { _lxmfDrafts[lxmfActiveContact] = input.value; }
                    else { delete _lxmfDrafts[lxmfActiveContact]; }
                }
                lxmfActiveContact = hash;
                if (input) { input.value = _lxmfDrafts[hash] || ''; input.style.height = ''; }
                container.querySelectorAll('.conv-row.active').forEach(function(r) { r.classList.remove('active'); });
                this.classList.add('active');
                _loadConversation(hash);
                loadConversations();
                if (window.innerWidth <= 768) {
                    RS.viewStack.push('chat-detail', { meta: { contactHash: hash } });
                    history.pushState({ view: 'message', detail: true }, '', '#message');
                }
            });
        });

        // Re-inject ghost row if the server list doesn't yet include the conversation.
        if (_ghostConversationHash && _ghostConversationHash === lxmfActiveContact) {
            if (!container.querySelector('.conv-row[data-hash="' + _ghostConversationHash + '"]')) {
                _ensureGhostRow(_ghostConversationHash);
            }
        }

    if (!container._convSwipeAttached) {
        container._convSwipeAttached = true;
        RS.gestures.attachSwipe(container, {
            delegated: '.conv-row',
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
                var nameEl = row.querySelector('.conv-name');
                var name = nameEl ? nameEl.textContent : (hash ? (typeof shortHash === 'function' ? shortHash(hash, 8, 4) : hash.substring(0, 12)) : 'this conversation');
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
}

function renderContactList() {
    var container = document.getElementById('lxmf-contacts');
    if (!container) return;

    if (lxmfContacts.length === 0) {
        container.innerHTML = '<div class="empty-state">' +
            '<svg class="empty-state-svg" width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/><path d="M23 21v-2a4 4 0 0 0-3-3.87"/><path d="M16 3.13a4 4 0 0 1 0 7.75"/></svg>' +
            '<span class="empty-state-primary">No contacts yet</span>' +
            '<span class="empty-state-hint">Add a contact to start a conversation</span>' +
        '</div>';
        return;
    }

    var sorted = lxmfContacts.slice().sort(function(a, b) {
        var na = (a.display_name || '').toLowerCase();
        var nb = (b.display_name || '').toLowerCase();
        if (na < nb) return -1;
        if (na > nb) return 1;
        return 0;
    });

    var html = '';
    var lastLetter = '';
    sorted.forEach(function(c) {
        var name = c.display_name || 'Anonymous';
        var firstChar = name.charAt(0).toUpperCase();
        var letter = /[A-Z]/.test(firstChar) ? firstChar : '#';
        if (letter !== lastLetter) {
            html += '<div class="contact-letter-separator">' + letter + '</div>';
            lastLetter = letter;
        }
        var activeClass = (lxmfActiveContact === c.hash) ? ' active' : '';
        // Prefer transport route/activity; fall back to identity-known.
        var reachStatus = 'unknown';
        var reachTitle = 'Unknown — no path data';
        var hopBadge = '';
        var peer = _peerInfo(c.hash);
        if (peer) {
            reachStatus = peer.status || 'unknown';
            reachTitle = _peerActivityLabel(peer) + ' - ' + _peerRouteLabel(peer);
            if (peer.hops !== null && peer.hops !== undefined) {
                hopBadge = '<span class="contact-hop-badge">' + peer.hops + (peer.hops === 1 ? ' hop' : ' hops') + '</span>';
            }
        } else {
            var idStatus = contactIdentityStatus[c.hash] || 'unknown';
            reachStatus = idStatus === 'known' ? 'reachable' : 'unknown';
            reachTitle = idStatus === 'known' ? 'Identity known' : 'Identity unknown — announce needed';
        }
        html += '<div class="lxmf-contact' + activeClass + '" data-hash="' + escapeHtml(c.hash) + '" tabindex="0" role="button">' +
            '<span class="contact-id-status status-' + reachStatus + '" title="' + reachTitle + '" role="img" aria-label="' + reachTitle + '"></span>' +
            '<span class="lxmf-contact-name">' + ratspeakDisplayNameHtml(name, c) + hopBadge + '</span>' +
            '<button class="lxmf-contact-remove" data-hash="' + escapeHtml(c.hash) + '" title="Remove contact">&times;</button>' +
        '</div>';
    });
    container.innerHTML = html;

    container.querySelectorAll('.lxmf-contact').forEach(function(el) {
        function activateContact() {
            var hash = el.dataset.hash;
            lxmfActiveContact = hash;
            renderContactList();
            _loadConversation(hash);
        }
        el.addEventListener('click', activateContact);
        el.addEventListener('keydown', function(e) {
            if (e.key === 'Enter' || e.key === ' ') {
                e.preventDefault();
                activateContact();
            }
        });
    });

    container.querySelectorAll('.lxmf-contact-remove').forEach(function(btn) {
        btn.addEventListener('click', function(e) {
            e.stopPropagation();
            var hash = this.dataset.hash;
            var contact = lxmfContacts.find(function(c) { return c.hash === hash; });
            var name = contact ? (contact.display_name || 'Anonymous') : (typeof shortHash === 'function' ? shortHash(hash, 8, 4) : hash.substring(0, 12));
            rsConfirm({ message: 'Remove contact "' + name + '"?', danger: true, confirmText: 'Remove' }).then(function(ok) {
                if (!ok) return;
                RS.invoke('remove_contact', { hash: hash }).catch(function() {});
                if (lxmfActiveContact === hash) {
                    lxmfActiveContact = null;
                    lxmfConversation = [];
                    renderConversation();
                }
            });
        });
    });
}

function renderStandaloneContactList() {
    var container = document.getElementById('contacts-standalone-list');
    if (!container) return;

    if (!container._ptrAttached) {
        RS.gestures.attachPullToRefresh(container, { onRefresh: renderStandaloneContactList });
    }

    var countEl = document.getElementById('contacts-count');

    if (lxmfContacts.length === 0) {
        if (countEl) countEl.textContent = '0 contacts';
        container.innerHTML = '<div class="empty-state">' +
            '<svg class="empty-state-svg" width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/><path d="M23 21v-2a4 4 0 0 0-3-3.87"/><path d="M16 3.13a4 4 0 0 1 0 7.75"/></svg>' +
            '<span class="empty-state-primary">No contacts yet</span>' +
            '<span class="empty-state-hint">Add a contact from Peers, or tap +</span>' +
        '</div>';
        return;
    }

    var searchInput = document.getElementById('contacts-search');
    var query = searchInput ? searchInput.value.toLowerCase().trim() : '';

    var sorted = lxmfContacts.slice().sort(function(a, b) {
        var la = contactSectionLetter(a);
        var lb = contactSectionLetter(b);
        var aIsHash = la === '#';
        var bIsHash = lb === '#';
        if (aIsHash !== bIsHash) return aIsHash ? 1 : -1;
        var na = (a.display_name || '').toLowerCase();
        var nb = (b.display_name || '').toLowerCase();
        if (na < nb) return -1;
        if (na > nb) return 1;
        return 0;
    });

    if (query) {
        sorted = sorted.filter(function(c) {
            var name = (c.display_name || '').toLowerCase();
            return name.indexOf(query) !== -1 || c.hash.toLowerCase().indexOf(query) !== -1;
        });
    }

    if (sorted.length === 0) {
        container.innerHTML = '<div class="empty-state"><span class="empty-state-primary">No matches</span></div>';
        return;
    }

    if (countEl) countEl.textContent = lxmfContacts.length + (lxmfContacts.length === 1 ? ' contact' : ' contacts');

    var html = '';
    var lastLetter = null;
    sorted.forEach(function(c) {
        var letter = contactSectionLetter(c);
        if (letter !== lastLetter) {
            html += '<div class="contacts-group-header">' + escapeHtml(letter) + '</div>';
            lastLetter = letter;
        }
        var name = c.display_name || 'Anonymous';
        var avatarHtml = '<span class="contacts-avatar">' + identityAvatar(c.hash, 40) + '</span>';
        html += '<div class="contacts-row" data-hash="' + escapeHtml(c.hash) + '">' +
            avatarHtml +
            '<div class="contacts-row-content">' +
                '<span class="contacts-row-name">' + ratspeakDisplayNameHtml(name, c) + '</span>' +
                '<span class="contacts-row-hash">' + escapeHtml(c.hash) + '</span>' +
            '</div>' +
        '</div>';
    });
    container.innerHTML = html;

    container.querySelectorAll('.contacts-row').forEach(function(el) {
        el.addEventListener('click', function() {
            showContactDetailSheet(this.dataset.hash);
        });
    });
}

function renderNetworkContactList() {
    var container = document.getElementById('dashboard-contacts-list');
    if (!container) return;

    if (lxmfContacts.length === 0) {
        container.innerHTML = '<div class="empty-state p-10"><span class="empty-state-primary">No contacts yet</span></div>';
        return;
    }

    var sorted = lxmfContacts.slice().sort(function(a, b) {
        var na = (a.display_name || '').toLowerCase();
        var nb = (b.display_name || '').toLowerCase();
        return na < nb ? -1 : na > nb ? 1 : 0;
    });

    var html = '';
    sorted.forEach(function(c) {
        var name = c.display_name || 'Anonymous';
        var truncHash = typeof shortHash === 'function' ? shortHash(c.hash, 8, 4) : (c.hash.length > 12 ? c.hash.substring(0, 6) + '\u2026' + c.hash.substring(c.hash.length - 4) : c.hash);
        var avatarHtml = '<span class="contacts-avatar">' + identityAvatar(c.hash, 32) + '</span>';
        html += '<div class="contacts-row" data-hash="' + escapeHtml(c.hash) + '" style="padding:8px 12px;min-height:40px;">' +
            avatarHtml +
            '<div class="contacts-row-content">' +
                '<span class="contacts-row-name">' + ratspeakDisplayNameHtml(name, c) + '</span>' +
                '<span class="contacts-row-hash">' + escapeHtml(truncHash) + '</span>' +
            '</div>' +
        '</div>';
    });
    container.innerHTML = html;

    container.querySelectorAll('.contacts-row').forEach(function(el) {
        el.addEventListener('click', function() { showContactDetailSheet(this.dataset.hash); });
    });
}

document.addEventListener('DOMContentLoaded', function() {
    var addBtn = document.getElementById('dashboard-contacts-add-btn');
    if (addBtn) {
        addBtn.addEventListener('click', function(e) {
            if (typeof openContactAddOptions === 'function') {
                openContactAddOptions(addBtn);
                return;
            }
            rsPromptContact({ title: 'Add Contact' }).then(function(result) {
                if (!result) return;
                RS.invoke('add_contact', { args: { hash: result.hash, display_name: result.display_name } }).catch(function() {});
                showToast('Adding contact...', 'toast-orange', 2000);
            });
        });
    }
    var dashAnnounce = document.getElementById('dash-announce');
    if (dashAnnounce) {
        dashAnnounce.addEventListener('click', function() {
            tryTriggerAnnounce();
        });
    }
    var dashNewMessage = document.getElementById('dash-new-message');
    if (dashNewMessage) {
        dashNewMessage.addEventListener('click', function() {
            switchView('message');
        });
    }
    var dashAddConnection = document.getElementById('dash-add-connection');
    if (dashAddConnection) {
        dashAddConnection.addEventListener('click', function() {
            switchView('network');
        });
    }
    var dashViewAllMessages = document.getElementById('dash-view-all-messages');
    if (dashViewAllMessages) {
        dashViewAllMessages.addEventListener('click', function() {
            switchView('message');
        });
    }
    var dashViewNetwork = document.getElementById('dash-view-network');
    if (dashViewNetwork) {
        dashViewNetwork.addEventListener('click', function() {
            switchView('peers');
        });
    }
});

function showContactDetailSheet(hash) {
    var existing = document.getElementById('contact-detail-overlay');
    if (existing) existing.remove();
    var existingSheet = document.getElementById('contact-detail-sheet');
    if (existingSheet) existingSheet.remove();

    var contact = lxmfContacts.find(function(c) { return c.hash === hash; });
    var name = contact ? (contact.display_name || 'Anonymous') : (typeof shortHash === 'function' ? shortHash(hash, 8, 4) : hash.substring(0, 4) + '...' + hash.substring(hash.length - 4));

    var reachStatus = 'unknown';
    var peer = _peerInfo(hash);
    var routeLabel = _peerRouteLabel(peer);
    var hopsText = peer && peer.hops !== null && peer.hops !== undefined
        ? peer.hops + (peer.hops === 1 ? ' hop' : ' hops')
        : '\u2014';
    var lastHeardText = _peerLastHeardLabel(peer);
    var firstHeardText = _peerFirstHeardLabel(peer);
    var pathAgeText = _peerPathAgeLabel(peer);
    var viaText = _peerViaLabel(peer);
    var ifaceText = _peerInterfaceLabel(peer);
    if (peer) reachStatus = peer.status || 'unknown';

    var avatarSvg = identityAvatar(hash, 64);

    var overlay = document.createElement('div');
    overlay.id = 'contact-detail-overlay';
    overlay.className = 'bottom-sheet-overlay';

    var sheet = document.createElement('div');
    sheet.id = 'contact-detail-sheet';
    sheet.className = 'bottom-sheet';
    sheet.innerHTML =
        '<div class="bottom-sheet-handle"></div>' +
        '<div class="bottom-sheet-body">' +
            '<button class="contact-detail-edit-btn" id="cd-edit-name-btn" title="Edit display name" aria-label="Edit display name">' +
                '<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 20h9"/><path d="M16.5 3.5a2.121 2.121 0 0 1 3 3L7 19l-4 1 1-4z"/></svg>' +
            '</button>' +
            '<div class="contact-detail-avatar">' + avatarSvg + '</div>' +
            '<div class="contact-detail-name">' + ratspeakDisplayNameHtml(name, contact || hash) + '</div>' +
            '<div class="contact-detail-hash-row">' +
                '<span class="contact-detail-hash">' + escapeHtml(hash) + '</span>' +
                '<button class="contact-detail-copy-btn" id="cd-copy-hash-btn" title="Copy LXMF address" aria-label="Copy LXMF address">' +
                    '<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>' +
                '</button>' +
            '</div>' +
            '<div class="contact-detail-actions contact-detail-primary-actions entity-action-grid">' +
                voiceActionButtonHtml('cd-call-btn', hash) +
                '<button class="nr-btn entity-action-btn" id="cd-message-btn"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/></svg><span>Message</span></button>' +
            '</div>' +
            '<div class="contact-detail-fields">' +
                '<div class="contact-detail-field">' +
                    '<span class="contact-detail-field-label">Last heard</span>' +
                    '<span class="contact-detail-field-value"><span class="contact-id-status status-' + reachStatus + '"></span> ' + escapeHtml(lastHeardText) + '</span>' +
                '</div>' +
                '<div class="contact-detail-field">' +
                    '<span class="contact-detail-field-label">First heard</span>' +
                    '<span class="contact-detail-field-value">' + escapeHtml(firstHeardText) + '</span>' +
                '</div>' +
                '<div class="contact-detail-field">' +
                    '<span class="contact-detail-field-label">Route</span>' +
                    '<span class="contact-detail-field-value">' + escapeHtml(routeLabel) + '</span>' +
                '</div>' +
                '<div class="contact-detail-field">' +
                    '<span class="contact-detail-field-label">Hops</span>' +
                    '<span class="contact-detail-field-value">' + escapeHtml(hopsText) + '</span>' +
                '</div>' +
                '<div class="contact-detail-field">' +
                    '<span class="contact-detail-field-label">Path age</span>' +
                    '<span class="contact-detail-field-value">' + escapeHtml(pathAgeText) + '</span>' +
                '</div>' +
                '<div class="contact-detail-field">' +
                    '<span class="contact-detail-field-label">Via</span>' +
                    '<span class="contact-detail-field-value">' + escapeHtml(viaText) + '</span>' +
                '</div>' +
                '<div class="contact-detail-field">' +
                    '<span class="contact-detail-field-label">Interface</span>' +
                    '<span class="contact-detail-field-value">' + escapeHtml(ifaceText) + '</span>' +
                '</div>' +
            '</div>' +
            '<div class="contact-detail-actions contact-detail-danger-actions entity-action-grid">' +
                '<button class="danger-btn entity-action-btn" id="cd-remove-btn"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 6h18"/><path d="M8 6V4h8v2"/><path d="M19 6l-1 14H6L5 6"/></svg><span>Remove</span></button>' +
                '<button class="danger-btn entity-action-btn" id="cd-block-btn"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="4.93" y1="4.93" x2="19.07" y2="19.07"/></svg><span>Block</span></button>' +
            '</div>' +
        '</div>';

    document.body.appendChild(overlay);
    document.body.appendChild(sheet);
    // Force layout flush so the .open transition runs.
    // eslint-disable-next-line no-unused-expressions
    sheet.offsetHeight;
    sheet.classList.add('open');
    overlay.classList.add('active');

    function closeSheet() {
        sheet.classList.remove('open');
        overlay.classList.remove('active');
        setTimeout(function() {
            if (overlay.parentNode) overlay.remove();
            if (sheet.parentNode) sheet.remove();
        }, 200);
    }

    if (typeof initSheetSwipeDismiss === 'function') {
        initSheetSwipeDismiss('contact-detail-sheet', 'contact-detail-overlay', closeSheet);
    }

    // Icon is the only copy affordance; hash text remains selectable without copying.
    var copyBtn = document.getElementById('cd-copy-hash-btn');
    if (copyBtn) {
        copyBtn.addEventListener('click', function(ev) {
            ev.stopPropagation();
            if (navigator.clipboard) {
                navigator.clipboard.writeText(hash);
                showCopyConfirmationToast('Address');
            }
        });
    }

    // add_contact is upsert-by-hash; new display_name renames in place.
    var editBtn = document.getElementById('cd-edit-name-btn');
    if (editBtn) {
        editBtn.addEventListener('click', function(ev) {
            ev.stopPropagation();
            if (typeof rsPrompt !== 'function') return;
            var currentName = contact ? (contact.display_name || '') : '';
            rsPrompt({
                message: 'Display name:',
                defaultValue: currentName,
                placeholder: 'Display name',
            }).then(function(newName) {
                if (newName === null) return;
                var trimmed = newName.trim();
                RS.invoke('add_contact', { args: { hash: hash, display_name: trimmed || null } }).catch(function() {});
                closeSheet();
            });
        });
    }

    document.getElementById('cd-message-btn').addEventListener('click', function() {
        closeSheet();
        if (typeof openConversationWith === 'function') {
            openConversationWith(hash);
        } else {
            if (typeof switchView === 'function') switchView('message');
        }
    });
    wireVoiceActionButton('cd-call-btn', closeSheet);

    document.getElementById('cd-remove-btn').addEventListener('click', function() {
        closeSheet();
        rsConfirm({ message: 'Remove contact "' + name + '"?', danger: true, confirmText: 'Remove' }).then(function(ok) {
            if (!ok) return;
            RS.invoke('remove_contact', { hash: hash }).catch(function() {});
        });
    });

    document.getElementById('cd-block-btn').addEventListener('click', function() {
        closeSheet();
        rsConfirmWithCheckbox({
            message: 'Block "' + name + '"? They won\'t appear in your peers list and their messages will be hidden.',
            danger: true,
            confirmText: 'Block',
            checkboxLabel: 'Also block at the network layer (drop their packets entirely)',
            checkboxHelp: 'Stops their messages from reaching this device at all, instead of just hiding them. Useful for relay nodes. This affects all accounts on this device.',
            defaultChecked: false
        }).then(function(result) {
            if (!result.confirmed) return;
            RS.invoke('block_contact', { args: { hash: hash, escalate_to_blackhole: result.checked } })
                .then(function(resp) {
                    if (resp && resp.blackhole_pending && typeof showToast === 'function') {
                        showToast('Blocked. Network blackhole will activate on their next announce.', 'toast-orange', 5000);
                    }
                })
                .catch(function() {});
        });
    });
}

document.addEventListener('DOMContentLoaded', function() {
    var searchInput = document.getElementById('contacts-search');
    if (searchInput) {
        searchInput.addEventListener('input', function() {
            if (typeof renderStandaloneContactList === 'function') renderStandaloneContactList();
        });
        searchInput.addEventListener('keydown', function(e) {
            if (e.key === 'Enter') { e.preventDefault(); this.blur(); }
        });
    }
    function openAddContactPrompt(trigger) {
        if (typeof openContactAddOptions === 'function' && trigger) {
            openContactAddOptions(trigger);
            return;
        }
        rsPromptContact({ title: 'Add Contact' }).then(function(result) {
            if (!result) return;
            RS.invoke('add_contact', { args: { hash: result.hash, display_name: result.display_name } }).catch(function() {});
            showToast('Adding contact...', 'toast-orange', 2000);
        });
    }

    RS.gestures.bindViewFabClick('contacts-add-fab', function() {
        openAddContactPrompt(document.getElementById('contacts-add-fab'));
    });
    var contactsHeaderAddBtn = document.getElementById('contacts-add-btn');
    if (contactsHeaderAddBtn) contactsHeaderAddBtn.addEventListener('click', function() {
        openAddContactPrompt(contactsHeaderAddBtn);
    });
});

window.renderConversation = renderConversation;

function renderConversation(options) {
    options = options || {};
    var container = document.getElementById('lxmf-messages');
    if (!container) return;
    _dismissContextMenu();
    _detachMessageLongPressHandlers();
    _wireLxmfMessageScroll(container);
    var scrollState = _captureLxmfMessageScrollState(container);

    var composeBar = document.getElementById('lxmf-compose-bar');
    if (composeBar) composeBar.style.display = lxmfActiveContact ? '' : 'none';

    var header = document.getElementById('lxmf-chat-header');
    if (header) {
        if (lxmfActiveContact) {
            header.style.display = 'flex';
            var contact = _lookupContactRecord(lxmfActiveContact);
            var nameInfo = _conversationNameInfo(lxmfActiveContact, null, false);
            document.getElementById('lxmf-chat-header-name').innerHTML = ratspeakDisplayNameHtml(nameInfo.name, lxmfActiveContact);

            var statusEl = document.getElementById('lxmf-chat-header-status');
            var avatarEl = document.getElementById('lxmf-contact-avatar');
            var peer = _peerInfo(lxmfActiveContact);
            var statusText = _peerHeaderStatus(peer);
            var statusOnline = !!(peer && peer.route_state && peer.route_state !== 'none');
            if (statusEl) {
                statusEl.textContent = statusText;
                statusEl.style.display = statusText ? '' : 'none';
                statusEl.className = 'lxmf-chat-header-status' + (statusOnline ? ' is-online' : '');
            }
            if (avatarEl) {
                avatarEl.innerHTML = identityAvatar(lxmfActiveContact, 40);
                avatarEl.className = 'lxmf-chat-header-avatar' + (statusOnline ? ' online' : '');
            }
            var addBtn = document.getElementById('lxmf-chat-add-contact-btn');
            if (addBtn) {
                addBtn.style.display = contact ? 'none' : '';
            }
        } else {
            header.style.display = 'none';
        }
    }
    renderVoiceUi();

    if (!lxmfActiveContact) {
        container.innerHTML = '<div class="lxmf-empty">' +
            '<svg width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1" stroke-linecap="round" stroke-linejoin="round" style="opacity:0.15;"><path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/></svg>' +
            '<span class="empty-state-primary">Your messages</span>' +
            '<span class="empty-state-hint">Select a conversation or start a new one</span>' +
        '</div>';
        return;
    }

    if (lxmfConversation.length === 0) {
        var emptyNameInfo = _conversationNameInfo(lxmfActiveContact, null, false);
        var emptyAvatar = (typeof identityAvatar === 'function') ? identityAvatar(lxmfActiveContact, 48) : '';
        container.innerHTML = '<div class="lxmf-empty-conv">' +
            '<div style="opacity:0.6;">' + emptyAvatar + '</div>' +
            '<span class="lxmf-empty-conv-name">' + ratspeakDisplayNameHtml(emptyNameInfo.name, lxmfActiveContact) + '</span>' +
            '<span class="lxmf-empty-conv-hint">This is the beginning of your encrypted conversation.</span>' +
        '</div>';
        return;
    }

    lxmfConversation.forEach(function(msg) {
        if (msg.reactions) _msgReactions[msg.id] = msg.reactions;
    });

    var ourHash = lxmfIdentity ? (lxmfIdentity.hash || '') : '';

    var msgs = lxmfConversation;
    var htmlParts = [];
    var lastDateLabel = '';

    for (var mi = 0; mi < msgs.length; mi++) {
        var msg = msgs[mi];

        var dateLabel = _formatDateLabel(msg.timestamp);
        if (dateLabel !== lastDateLabel) {
            htmlParts.push('<div class="msg-date-separator"><span>' + escapeHtml(dateLabel) + '</span></div>');
            lastDateLabel = dateLabel;
        }

        // Group same-sender msgs <3 min apart for bubble-corner rounding.
        var sameSenderAsPrev = (mi > 0) && msgs[mi - 1].direction === msg.direction &&
            (msg.timestamp - msgs[mi - 1].timestamp < 180) && dateLabel === _formatDateLabel(msgs[mi - 1].timestamp);
        var sameSenderAsNext = (mi < msgs.length - 1) && msgs[mi + 1].direction === msg.direction &&
            (msgs[mi + 1].timestamp - msg.timestamp < 180) && _formatDateLabel(msgs[mi + 1].timestamp) === dateLabel;
        var groupClass = '';
        if (sameSenderAsPrev && sameSenderAsNext) groupClass = ' msg-group-middle';
        else if (sameSenderAsPrev) groupClass = ' msg-group-last';
        else if (sameSenderAsNext) groupClass = ' msg-group-first';

        var isOut = msg.direction === 'outbound';
        var bubbleClassBase = isOut ? 'lxmf-msg outbound' : 'lxmf-msg inbound';
        var time = formatTime(msg.timestamp);
        var stateIcon = isOut ? _messageStateIconHtml(msg) : '';
        var progressPercent = isOut ? _messageProgressPercent(msg) : null;
        var canCancelSend = isOut && _messageCanCancelTransfer(msg);

        var replyHtml = '';
        if (msg.reply_to_id || msg.reply_to_preview) {
            replyHtml = '<div class="msg-reply-quote" data-reply-id="' + escapeHtml(msg.reply_to_id || '') + '">' +
                '<span class="reply-quote-bar"></span>' +
                '<span class="reply-quote-text">' + escapeHtml(msg.reply_to_preview || '') + '</span>' +
            '</div>';
        }

        var attachHtml = '';
        if (msg.attachments && msg.attachments.length > 0) {
            attachHtml = msg.attachments.map(function(att) {
                var sizeStr = att.size ? prettySize(att.size) : '';
                var nameHtml = att.stored_name
                    ? '<a href="#" class="file-name rs-file-download" data-stored-name="' + escapeHtml(att.stored_name) + '">' + escapeHtml(att.filename || 'file') + '</a>'
                    : '<span class="file-name">' + escapeHtml(att.filename || 'file') + '</span>';
                return '<div class="file-transfer-info">' +
                    '<span class="file-icon">\ud83d\udcce</span>' + nameHtml +
                    '<span class="file-size">' + sizeStr + '</span>' +
                '</div>';
            }).join('');
        }

        var imageHtml = '';
        var imageSendOverlay = '';
        if (msg.image) {
            var imageFilename = msg.image.filename || 'image';
            var imageMime = msg.image.mime_type || msg.image.mime || '';
            var imageSendingClass = canCancelSend ? ' is-sending' : '';
            imageSendOverlay = canCancelSend ? _messageSendCancelOverlayHtml(msg, progressPercent) : '';
            // stored_name \u2192 async fetch via RS.fileDownload; data_url \u2192 embed direct.
            if (msg.image.stored_name) {
                imageHtml = '<div class="lxmf-msg-image' + imageSendingClass + '">' +
                    '<button type="button" class="lxmf-image-button" aria-label="Open image" ' +
                    'data-stored-name="' + escapeHtml(msg.image.stored_name) + '" ' +
                    'data-filename="' + escapeHtml(imageFilename) + '" ' +
                    'data-mime="' + escapeHtml(imageMime) + '">' +
                    '<img data-stored-name="' + escapeHtml(msg.image.stored_name) + '" alt="Image" ' +
                    'data-filename="' + escapeHtml(imageFilename) + '" ' +
                    'data-mime="' + escapeHtml(imageMime) + '" ' +
                    'class="lxmf-clickable-img rs-lazy-image">' +
                    '</button>' +
                    imageSendOverlay +
                '</div>';
            } else if (msg.image.data_url) {
                imageHtml = '<div class="lxmf-msg-image' + imageSendingClass + '">' +
                    '<button type="button" class="lxmf-image-button" aria-label="Open image" ' +
                    'data-filename="' + escapeHtml(imageFilename) + '" ' +
                    'data-mime="' + escapeHtml(imageMime) + '">' +
                    '<img src="' + msg.image.data_url + '" alt="Image" class="lxmf-clickable-img" ' +
                    'data-filename="' + escapeHtml(imageFilename) + '" ' +
                    'data-mime="' + escapeHtml(imageMime) + '">' +
                    '</button>' +
                    imageSendOverlay +
                '</div>';
            }
        }

        var reactionHtml = '';
        var reactions = _msgReactions[msg.id] || [];
        if (reactions.length > 0) {
            var grouped = {};
            reactions.forEach(function(r) {
                if (!grouped[r.emoji]) grouped[r.emoji] = [];
                grouped[r.emoji].push(r.sender);
            });
            reactionHtml = '<div class="msg-reactions">';
            Object.keys(grouped).forEach(function(emoji) {
                var count = grouped[emoji].length;
                var isMine = grouped[emoji].indexOf(ourHash) !== -1;
                reactionHtml += '<span class="reaction-pill' + (isMine ? ' mine' : '') + '" ' +
                    'data-emoji="' + escapeHtml(emoji) + '" data-msg-id="' + escapeHtml(msg.id) + '">' +
                    emoji + (count > 1 ? ' ' + count : '') + '</span>';
            });
            reactionHtml += '</div>';
        }

        // Strip the `[File: ...]` fallback suffix when we have structured payload.
        var displayContent = msg.content || '';
        if ((msg.image || (msg.attachments && msg.attachments.length > 0)) && displayContent) {
            displayContent = displayContent.replace(/\n?\[File:[^\]]*\]\s*$/, '');
        }

        var hasReactions = reactionHtml ? ' has-reactions' : '';
        var hasImage = !!imageHtml;
        var hasAttachment = !!attachHtml;
        var metaHtml = _messageProgressMetaHtml(msg) +
            '<span class="msg-time">' + time + '</span>' +
            (!hasImage ? _messageInlineCancelHtml(msg) : '') +
            stateIcon;
        var bubbleClass = bubbleClassBase +
            (hasImage ? ' msg-has-image' : '') +
            (hasImage && !displayContent && !hasAttachment ? ' msg-image-only' : '');
        htmlParts.push('<div class="msg-row' + (isOut ? ' outbound' : ' inbound') + hasReactions + groupClass + '">' +
            '<div class="' + bubbleClass + '" data-msg-id="' + escapeHtml(msg.id || '') + '">' +
                replyHtml +
                imageHtml +
                (displayContent ? '<div class="lxmf-msg-content">' + linkifyMessageText(displayContent) + '</div>' : '') +
                attachHtml +
                '<div class="lxmf-msg-meta">' + metaHtml + '</div>' +
            '</div>' +
            reactionHtml +
        '</div>');
    }
    container.innerHTML = htmlParts.join('');
    var shouldPinMessages = _applyLxmfMessageScrollAfterRender(container, scrollState, options);
    _watchLxmfImagesForBottomPin(container, shouldPinMessages);

    // Async swap blob URL into the data-stored-name placeholders.
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
            img.src = cachedUrl;
            return;
        }
        RS.fileDownload(name).then(function(f) {
            _rememberImageBlobUrl(name, f.url, f);
            if (f.filename) img.setAttribute('data-filename', f.filename);
            if (f.mime) img.setAttribute('data-mime', f.mime);
            var btn = img.closest('.lxmf-image-button');
            if (btn) {
                if (f.filename) btn.setAttribute('data-filename', f.filename);
                if (f.mime) btn.setAttribute('data-mime', f.mime);
            }
            img.src = f.url;
        }).catch(function(err) {
            window.RS.diag('warn', '[lxmf] inline image fetch failed:', name, err);
            img.classList.add('is-error');
            img.setAttribute('alt', 'Image failed to load');
        });
    });

    // Attachment click → save via IPC (no HTTP endpoint to download from).
    container.querySelectorAll('a.rs-file-download[data-stored-name]').forEach(function(link) {
        link.addEventListener('click', function(e) {
            e.preventDefault();
            var name = this.getAttribute('data-stored-name');
            if (!name) return;
            RS.saveFile(name).then(function() {
                if (typeof showToast === 'function') showToast('Saved', 'toast-green', 2200);
            }).catch(function(err) {
                if (typeof showToast === 'function') {
                    showToast('Download failed: ' + (err.message || err.code || 'error'), 'toast-red', 4000);
                } else {
                    window.RS.diag('error', '[lxmf] file download failed:', name, err);
                }
            });
        });
    });

    container.querySelectorAll('.lxmf-send-cancel, .msg-send-cancel-inline').forEach(function(btn) {
        btn.addEventListener('click', function(e) {
            e.preventDefault();
            e.stopPropagation();
            _cancelLxmfSend(this.getAttribute('data-msg-id'));
        });
    });

    container.querySelectorAll('.lxmf-clickable-img').forEach(function(img) {
        img.addEventListener('click', function(e) {
            e.stopPropagation();
            if (typeof openImageViewer === 'function') openImageViewer(this);
        });
    });

    container.querySelectorAll('.rs-message-link[data-url]').forEach(function(link) {
        link.addEventListener('click', function(e) {
            e.preventDefault();
            e.stopPropagation();
            var url = this.getAttribute('data-url');
            if (url && window.RS && typeof RS.openExternalUrl === 'function') {
                RS.openExternalUrl(url).catch(function(err) {
                    showToast('Could not open link: ' + ((err && err.message) || 'error'), 'toast-red', 3500);
                });
            }
        });
    });

    container.querySelectorAll('.msg-retry-btn').forEach(function(btn) {
        btn.addEventListener('click', function() {
            var dest = this.getAttribute('data-dest');
            var content = this.getAttribute('data-content');
            if (dest && content) {
                RS.invoke('send_lxmf_message', { args: { dest_hash: dest, content: content, delivery_method: 'auto' } }).catch(function() {});
                this.disabled = true;
                this.textContent = 'Retrying...';
            }
        });
    });

    container.querySelectorAll('.reaction-pill').forEach(function(pill) {
        pill.addEventListener('click', function(e) {
            e.stopPropagation();
            var emoji = this.getAttribute('data-emoji');
            var msgId = this.getAttribute('data-msg-id');
            var msgData = lxmfConversation.find(function(m) { return m.id === msgId; }) || { id: msgId };
            _sendReactionForMessage(msgData, emoji, { dismiss: false });
        });
    });

    container.querySelectorAll('.msg-reply-quote').forEach(function(quote) {
        quote.addEventListener('click', function() {
            var targetId = this.getAttribute('data-reply-id');
            if (!targetId) return;
            var targetEl = container.querySelector('[data-msg-id="' + targetId + '"]');
            if (targetEl) {
                var scrollBlock = (window.innerWidth <= 768) ? 'nearest' : 'center';
                setTimeout(function() {
                    targetEl.scrollIntoView({ behavior: 'smooth', block: scrollBlock });
                }, 100);
                targetEl.classList.add('msg-highlight');
                setTimeout(function() { targetEl.classList.remove('msg-highlight'); }, 1500);
            }
        });
    });

    container.querySelectorAll('.lxmf-msg').forEach(function(bubble) {
        if (window.RS && RS.gestures && typeof RS.gestures.attachLongPress === 'function') {
            var detachLongPress = RS.gestures.attachLongPress(bubble, {
                duration: 500,
                moveCancelPx: 12,
                hapticStages: [{ at: 0.55, level: 'light' }],
                onFire: function(touch) {
                    var msgId = bubble.getAttribute('data-msg-id');
                    if (!msgId) return;
                    var msgData = lxmfConversation.find(function(m) { return m.id === msgId; });
                    if (!msgData) return;
                    _suppressNextContextMenuUntil = Date.now() + 1200;
                    _showMsgContextMenu(msgData, touch.clientX, touch.clientY, bubble);
                }
            });
            if (typeof detachLongPress === 'function') _messageLongPressDetachFns.push(detachLongPress);
        }
        bubble.addEventListener('contextmenu', function(e) {
            e.preventDefault();
            if (Date.now() < _suppressNextContextMenuUntil) return;
            var msgId = this.getAttribute('data-msg-id');
            if (!msgId) return;
            var msgData = lxmfConversation.find(function(m) { return m.id === msgId; });
            if (msgData) _showMsgContextMenu(msgData, e.clientX, e.clientY, this);
        });
    });

}

(function() {
    var searchInput = document.getElementById('msg-search-input');
    var searchResults = document.getElementById('msg-search-results');
    var convoList = document.getElementById('lxmf-conversations-list');
    if (!searchInput) return;

    var searchTimer = null;
    searchInput.addEventListener('input', function() {
        var q = this.value.trim();
        if (searchTimer) clearTimeout(searchTimer);
        if (q.length < 2) {
            if (searchResults) searchResults.style.display = 'none';
            if (convoList) convoList.style.display = '';
            return;
        }
        searchResults.innerHTML = '<div class="lxmf-empty">Searching...</div>';
        searchResults.style.display = 'block';
        if (convoList) convoList.style.display = 'none';
        searchTimer = setTimeout(function() {
            RS.invoke('api_search_messages', { q: q })
                .then(function(results) {
                    if (!results || results.length === 0) {
                        searchResults.innerHTML = '<div class="lxmf-empty">No results found.</div>';
                    } else {
                        searchResults.innerHTML = results.map(function(msg) {
                            var other = msg.direction === 'inbound' ? msg.source : msg.destination;
                            var preview = (msg.content || '').substring(0, 80);
                            var time = formatTime(msg.timestamp);
                            var otherLabel = typeof shortHash === 'function' ? shortHash(other, 8, 4) : other.substring(0, 12) + '...';
                            return '<div class="lxmf-convo-item" data-hash="' + escapeHtml(other) + '">' +
                                '<div class="convo-name">' + escapeHtml(otherLabel) + '</div>' +
                                '<div class="convo-preview">' + escapeHtml(preview) + '</div>' +
                                '<div class="convo-time">' + time + '</div>' +
                            '</div>';
                        }).join('');
                        searchResults.querySelectorAll('.lxmf-convo-item').forEach(function(item) {
                            item.addEventListener('click', function() {
                                var hash = this.getAttribute('data-hash');
                                if (hash) openConversationWith(hash);
                                searchInput.value = '';
                                searchResults.style.display = 'none';
                                if (convoList) convoList.style.display = '';
                            });
                        });
                    }
                }).catch(function(err) {
                    window.RS.diag('error', 'Message search failed:', err);
                    searchResults.innerHTML = '<div class="lxmf-empty">Search failed.</div>';
                });
        }, 300);
    });
    searchInput.addEventListener('keydown', function(e) {
        if (e.key === 'Enter') { e.preventDefault(); this.blur(); }
    });
})();

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
    _lxmfSendInputWasFocused = !!(input && document.activeElement === input);
    _lxmfSendInputFocusCapturedAt = Date.now();
}

function _consumeLxmfSendFocusState(input) {
    var isFocusedNow = !!(input && document.activeElement === input);
    if (_lxmfSendInputWasFocused !== null && Date.now() - _lxmfSendInputFocusCapturedAt < 8000) {
        var shouldRestoreFocus = _lxmfSendInputWasFocused || isFocusedNow;
        _lxmfSendInputWasFocused = null;
        _lxmfSendInputFocusCapturedAt = 0;
        return shouldRestoreFocus;
    }
    _lxmfSendInputWasFocused = null;
    _lxmfSendInputFocusCapturedAt = 0;
    return isFocusedNow;
}

function _focusLxmfComposerInput(input) {
    try {
        input.focus({ preventScroll: true });
    } catch (_) {
        input.focus();
    }
}

function _finishLxmfComposerSend(input, shouldRestoreFocus) {
    input.value = '';
    input.style.height = '';
    input.scrollTop = 0;
    delete _lxmfDrafts[lxmfActiveContact];
    if (shouldRestoreFocus) {
        _focusLxmfComposerInput(input);
    } else if (document.activeElement === input) {
        input.blur();
    }
    var counter = document.getElementById('lxmf-char-count');
    if (counter) { counter.textContent = ''; counter.className = 'lxmf-char-count'; counter.style.display = 'none'; }
}

function sendLxmfMessage(deliveryMethod) {
    if (!lxmfActiveContact) return;
    var input = document.getElementById('lxmf-input');
    if (!input) return;
    var shouldRestoreComposerFocus = _consumeLxmfSendFocusState(input);
    var text = input.value.trim();
    var chosenDelivery = _deliveryPrefOrAuto(deliveryMethod);
    var maxMessageBytes = lxmfLimits.max_message_bytes || 134217727;
    if (text && _utf8ByteLength(text) > maxMessageBytes) {
        showToast('Message exceeds protocol limit (' + prettySize(_utf8ByteLength(text)) + ' > ' + prettySize(maxMessageBytes) + ').', 'toast-red', 5000);
        return;
    }

    if (lxmfPendingFile) {
        var attachMsgId = generateMsgId();
        var isImage = lxmfPendingFile.mime && lxmfPendingFile.mime.startsWith('image/');
        RS.invoke('send_lxmf_with_attachment', {
            args: {
                dest_hash: lxmfActiveContact,
                content: text,
                delivery_method: chosenDelivery,
                client_msg_id: attachMsgId,
                file_data: isImage ? null : lxmfPendingFile.data,
                file_name: lxmfPendingFile.name,
                image_data: isImage ? lxmfPendingFile.data : null,
                image_mime: isImage ? lxmfPendingFile.mime : null,
            }
        }).then(function(resp) {
            _handleLxmfSendAccepted(resp, attachMsgId);
        }).catch(function() {});

        lxmfConversation.push({
            id: attachMsgId,
            direction: 'outbound',
            content: text,
            timestamp: Date.now() / 1000,
            state: 'sending',
            delivery_method: _optimisticDeliveryMethod(chosenDelivery),
            attachments: isImage ? [] : [{ filename: lxmfPendingFile.name, size: lxmfPendingFile.size }],
            image: isImage ? {
                mime_type: lxmfPendingFile.mime,
                size: lxmfPendingFile.size,
                filename: lxmfPendingFile.name,
                data_url: 'data:' + lxmfPendingFile.mime + ';base64,' + lxmfPendingFile.data,
            } : null,
        });

        // Capture before clearPendingFile() wipes pending state.
        var attachPreview = text || (isImage ? 'Photo' : lxmfPendingFile.name);
        clearPendingFile();
        renderConversation({ forceScrollBottom: true });
        _updateConversationPreview(lxmfActiveContact, attachPreview, Date.now() / 1000);
        loadConversations();
        _finishLxmfComposerSend(input, shouldRestoreComposerFocus);
        return;
    }

    if (!text) return;

    var msgId = generateMsgId();

    if (_replyTarget) {
        RS.invoke('send_lxmf_reply', {
            args: {
                dest_hash: lxmfActiveContact,
                content: text,
                delivery_method: chosenDelivery,
                reply_to_id: _replyTarget.id,
                reply_to_preview: _replyTarget.content,
                client_msg_id: msgId,
            }
        }).then(function(resp) {
            _handleLxmfSendAccepted(resp, msgId);
        }).catch(function() {});
        lxmfConversation.push({
            id: msgId,
            direction: 'outbound',
            content: text,
            timestamp: Date.now() / 1000,
            state: 'sending',
            delivery_method: _optimisticDeliveryMethod(chosenDelivery),
            reply_to_id: _replyTarget.id,
            reply_to_preview: _replyTarget.content,
        });
        clearReplyTarget();
        renderConversation({ forceScrollBottom: true });
        _updateConversationPreview(lxmfActiveContact, text, Date.now() / 1000);
        loadConversations();
        _finishLxmfComposerSend(input, shouldRestoreComposerFocus);
        return;
    }

    RS.invoke('send_lxmf_message', {
        args: {
            dest_hash: lxmfActiveContact,
            content: text,
            delivery_method: chosenDelivery,
            client_msg_id: msgId,
        }
    }).then(function(resp) {
        _handleLxmfSendAccepted(resp, msgId);
    }).catch(function() {});

    lxmfConversation.push({
        id: msgId,
        direction: 'outbound',
        content: text,
        timestamp: Date.now() / 1000,
        state: 'sending',
        delivery_method: _optimisticDeliveryMethod(chosenDelivery),
    });
    renderConversation({ forceScrollBottom: true });
    _updateConversationPreview(lxmfActiveContact, text, Date.now() / 1000);
    loadConversations();
    _finishLxmfComposerSend(input, shouldRestoreComposerFocus);
}

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
            showToast(message, 'toast-orange', 3500);
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

function _imageShareMime(mime) {
    var lower = String(mime || '').toLowerCase();
    if (lower === 'image/jpeg' || lower === 'image/jpg') return 'image/jpeg';
    if (lower === 'image/webp') return 'image/webp';
    return 'image/png';
}

function _imageShareExtension(mime) {
    if (mime === 'image/jpeg') return 'jpg';
    if (mime === 'image/webp') return 'webp';
    return 'png';
}

function _metadataStrippedImageName(file, mime) {
    var source = _pendingAttachmentName(file);
    var stem = source.replace(/\.[A-Za-z0-9]{1,8}$/, '').trim() || 'image';
    return stem + '.' + _imageShareExtension(mime);
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

function _decodeImageForCanvas(file) {
    if (typeof createImageBitmap === 'function') {
        return createImageBitmap(file, { imageOrientation: 'from-image' }).then(function(bitmap) {
            return {
                source: bitmap,
                width: bitmap.width,
                height: bitmap.height,
                close: function() {
                    if (typeof bitmap.close === 'function') bitmap.close();
                }
            };
        }).catch(function() {
            return _decodeImageElementForCanvas(file);
        });
    }
    return _decodeImageElementForCanvas(file);
}

function _decodeImageElementForCanvas(file) {
    return new Promise(function(resolve, reject) {
        var url = URL.createObjectURL(file);
        var img = new Image();
        img.onload = function() {
            URL.revokeObjectURL(url);
            resolve({
                source: img,
                width: img.naturalWidth || img.width,
                height: img.naturalHeight || img.height,
                close: function() {}
            });
        };
        img.onerror = function() {
            URL.revokeObjectURL(url);
            reject(new Error('Could not decode image'));
        };
        img.src = url;
    });
}

function _canvasToBlob(canvas, mime) {
    return new Promise(function(resolve, reject) {
        try {
            canvas.toBlob(function(blob) {
                if (!blob) {
                    reject(new Error('Could not encode sanitized image'));
                    return;
                }
                resolve(blob);
            }, mime, 0.92);
        } catch (err) {
            reject(err);
        }
    });
}

function _stripImageMetadataForShare(file) {
    var targetMime = _imageShareMime(file && file.type);
    return _decodeImageForCanvas(file).then(function(decoded) {
        if (!decoded.width || !decoded.height) {
            if (decoded.close) decoded.close();
            throw new Error('Image has no readable pixels');
        }
        var canvas = document.createElement('canvas');
        canvas.width = decoded.width;
        canvas.height = decoded.height;
        var ctx = canvas.getContext('2d', { alpha: targetMime !== 'image/jpeg' });
        if (!ctx) {
            if (decoded.close) decoded.close();
            throw new Error('Could not prepare image sanitizer');
        }
        if (targetMime === 'image/jpeg') {
            ctx.fillStyle = '#fff';
            ctx.fillRect(0, 0, canvas.width, canvas.height);
        }
        ctx.drawImage(decoded.source, 0, 0, canvas.width, canvas.height);
        if (decoded.close) decoded.close();
        return _canvasToBlob(canvas, targetMime);
    }).then(function(blob) {
        return _readBlobBase64(blob).then(function(base64) {
            return {
                name: _metadataStrippedImageName(file, targetMime),
                data: base64,
                size: blob.size,
                mime: targetMime,
                metadata_stripped: true
            };
        });
    });
}

function _readGenericAttachment(file) {
    return _readBlobBase64(file).then(function(base64) {
        return {
            name: _pendingAttachmentName(file),
            data: base64,
            size: file.size,
            mime: file.type || 'application/octet-stream',
        };
    });
}

function handleFileSelected(inputEl) {
    var file = inputEl.files[0];
    if (!file) return;

    var maxSize = lxmfLimits.max_attachment_bytes || 134217727;
    if (file.size > maxSize) {
        showToast('File exceeds protocol limit (' + prettySize(file.size) + ' > ' + prettySize(maxSize) + '). Choose a smaller file.', 'toast-red', 5000);
        inputEl.value = '';
        clearPendingFile();
        return;
    }
    if (file.size > (lxmfLimits.efficient_resource_bytes || 1048575)) {
        showToast('Large attachment - transfer may take a while on slow links.', 'toast-blue', 3500);
    }

    var token = ++_pendingAttachmentToken;
    var isImage = /^image\//i.test(file.type || '');
    var prepare = isImage ? _stripImageMetadataForShare(file) : _readGenericAttachment(file);
    if (isImage) showToast('Removing image metadata...', 'toast-blue', 1800);

    prepare.then(function(pendingFile) {
        if (token !== _pendingAttachmentToken) return;
        if (pendingFile.size > maxSize) {
            showToast('Sanitized image exceeds protocol limit (' + prettySize(pendingFile.size) + ' > ' + prettySize(maxSize) + '). Choose a smaller image.', 'toast-red', 5000);
            clearPendingFile();
            return;
        }
        if (pendingFile.size > (lxmfLimits.efficient_resource_bytes || 1048575) && file.size <= (lxmfLimits.efficient_resource_bytes || 1048575)) {
            showToast('Large attachment - transfer may take a while on slow links.', 'toast-blue', 3500);
        }
        lxmfPendingFile = pendingFile;
        renderPendingFile();
    }).catch(function(err) {
        if (token !== _pendingAttachmentToken) return;
        clearPendingFile();
        if (isImage) {
            showToast('Could not remove image metadata; image not attached', 'toast-red', 4500);
        } else {
            showToast('Could not read attachment: ' + ((err && err.message) || 'error'), 'toast-red', 4500);
        }
    });
    inputEl.value = '';
}

function renderPendingFile() {
    var container = document.getElementById('lxmf-pending-file');
    if (!container) return;

    if (!lxmfPendingFile) {
        container.innerHTML = '';
        container.style.display = 'none';
        return;
    }

    container.style.display = 'flex';
    var isImage = lxmfPendingFile.mime.startsWith('image/');
    container.classList.toggle('pending-file-has-image', isImage);
    var previewHtml = isImage
        ? '<span class="pending-file-thumbnail"><img src="data:' + escapeHtml(lxmfPendingFile.mime) + ';base64,' + lxmfPendingFile.data + '" alt=""></span>'
        : '<span class="pending-file-thumbnail pending-file-thumbnail-file"><span class="file-icon">\ud83d\udcce</span></span>';
    container.innerHTML =
        previewHtml +
        '<span class="pending-file-copy">' +
            '<span class="file-name">' + escapeHtml(lxmfPendingFile.name) + '</span>' +
            '<span class="file-size">' + prettySize(lxmfPendingFile.size) + '</span>' +
        '</span>' +
        '<button class="pending-file-clear">&times;</button>';
    container.querySelector('.pending-file-clear').addEventListener('click', clearPendingFile);
}

function clearPendingFile() {
    _pendingAttachmentToken++;
    lxmfPendingFile = null;
    var container = document.getElementById('lxmf-pending-file');
    if (container) {
        container.innerHTML = '';
        container.style.display = 'none';
        container.classList.remove('pending-file-has-image');
    }
}

function setReplyTarget(msgData) {
    _replyTarget = {
        id: msgData.id,
        content: (msgData.content || '').substring(0, 100),
        sender: msgData.direction === 'inbound' ? msgData.source : msgData.destination,
        senderName: msgData.direction === 'inbound' ? _getContactName(msgData.source) : 'You',
    };
    var bar = document.getElementById('lxmf-reply-preview');
    if (bar) {
        bar.querySelector('.reply-preview-sender').textContent = _replyTarget.senderName;
        bar.querySelector('.reply-preview-text').textContent = _replyTarget.content;
        bar.style.display = 'flex';
    }
    var input = document.getElementById('lxmf-input');
    if (input) input.focus();
}

function clearReplyTarget() {
    _replyTarget = null;
    var bar = document.getElementById('lxmf-reply-preview');
    if (bar) bar.style.display = 'none';
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
var _suppressNextContextMenuUntil = 0;

function _dismissContextMenu() {
    if (!_activeContextMenu) return false;
    if (_activeContextMenu.menu && _activeContextMenu.menu.parentNode) {
        _activeContextMenu.menu.parentNode.removeChild(_activeContextMenu.menu);
    }
    if (_activeContextMenu.row) {
        _activeContextMenu.row.classList.remove('msg-action-selected');
    }
    if (_activeContextMenu.container) {
        _activeContextMenu.container.classList.remove('msg-action-mode');
    }
    _activeContextMenu = null;
    return true;
}

function _messageActionIcon(name) {
    if (name === 'reply') {
        return '<svg viewBox="0 0 24 24" aria-hidden="true"><polyline points="9 17 4 12 9 7"/><path d="M20 18v-2a4 4 0 0 0-4-4H4"/></svg>';
    }
    if (name === 'copy') {
        return '<svg viewBox="0 0 24 24" aria-hidden="true"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>';
    }
    return '';
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

function _sendReactionForMessage(msgData, emoji, opts) {
    opts = opts || {};
    if (!msgData || !msgData.id || !emoji) return;
    var action = _hasOwnReaction(msgData.id, emoji) ? 'remove' : 'add';
    if (opts.dismiss !== false) _dismissContextMenu();
    _optimisticApplyReaction(msgData.id, emoji, action);
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
        if (typeof showToast === 'function') showToast('Reaction failed', 'toast-red', 2500);
    });
}

function _copyToClipboard(text) {
    if (!text) return Promise.resolve(false);
    if (navigator.clipboard && navigator.clipboard.writeText) {
        return navigator.clipboard.writeText(text).then(function() { return true; }).catch(function() {
            return _copyToClipboardFallback(text);
        });
    }
    return _copyToClipboardFallback(text);
}

function _copyToClipboardFallback(text) {
    var ta = document.createElement('textarea');
    ta.value = text;
    ta.setAttribute('readonly', '');
    ta.style.position = 'fixed';
    ta.style.left = '-9999px';
    ta.style.top = '0';
    document.body.appendChild(ta);
    ta.select();
    var ok = false;
    try { ok = document.execCommand('copy'); } catch (e) { ok = false; }
    if (ta.parentNode) ta.parentNode.removeChild(ta);
    return Promise.resolve(ok);
}

function _positionMsgContextMenu(menu, x, y, bubble) {
    var margin = 10;
    var rect = bubble ? bubble.getBoundingClientRect() : null;
    var menuRect = menu.getBoundingClientRect();
    var isMobileWidth = window.innerWidth <= 768;
    var left = x;
    var top = y;

    if (rect) {
        left = isMobileWidth ? (window.innerWidth - menuRect.width) / 2 : x;
        var above = rect.top - menuRect.height - 8;
        var below = rect.bottom + 8;
        top = above >= margin ? above : below;
    }

    left = Math.max(margin, Math.min(left, window.innerWidth - menuRect.width - margin));
    top = Math.max(margin, Math.min(top, window.innerHeight - menuRect.height - margin));
    menu.style.left = left + 'px';
    menu.style.top = top + 'px';
}

function _showMsgContextMenu(msgData, x, y, bubble) {
    _dismissContextMenu();

    if (typeof haptic === 'function') haptic('selection');
    var container = document.getElementById('lxmf-messages');
    var row = bubble && bubble.closest ? bubble.closest('.msg-row') : null;
    if (!row && container && msgData.id) {
        var target = container.querySelector('[data-msg-id="' + msgData.id + '"]');
        row = target && target.closest ? target.closest('.msg-row') : null;
    }
    if (container) container.classList.add('msg-action-mode');
    if (row) row.classList.add('msg-action-selected');

    var menu = document.createElement('div');
    menu.className = 'msg-context-menu msg-action-menu';
    menu.setAttribute('role', 'menu');

    var reactBar = document.createElement('div');
    reactBar.className = 'msg-quick-react';
    var quickEmojis = ['\u2764\uFE0F', '\uD83D\uDC4D', '\uD83D\uDE02', '\uD83D\uDE2E', '\uD83D\uDE22', '\uD83D\uDD25'];
    quickEmojis.forEach(function(em) {
        var btn = document.createElement('button');
        btn.type = 'button';
        btn.className = 'quick-react-emoji';
        btn.textContent = em;
        btn.setAttribute('aria-label', 'React ' + em);
        btn.addEventListener('click', function(e) {
            e.stopPropagation();
            _sendReactionForMessage(msgData, em);
        });
        reactBar.appendChild(btn);
    });

    var plusBtn = document.createElement('button');
    plusBtn.type = 'button';
    plusBtn.className = 'quick-react-plus';
    plusBtn.textContent = '+';
    plusBtn.title = 'More emoji';
    plusBtn.setAttribute('aria-label', 'More emoji');
    plusBtn.addEventListener('click', function(e) {
        e.stopPropagation();
        // Capture before dismiss removes plusBtn from the DOM.
        var btnRect = plusBtn.getBoundingClientRect();
        _dismissContextMenu();
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
    actions.className = 'msg-context-actions';

    var replyBtn = document.createElement('button');
    replyBtn.className = 'msg-ctx-btn msg-ctx-reply';
    replyBtn.type = 'button';
    replyBtn.innerHTML = _messageActionIcon('reply') + '<span>Reply</span>';
    replyBtn.addEventListener('click', function(e) {
        e.stopPropagation();
        _dismissContextMenu();
        setReplyTarget(msgData);
        if (typeof haptic === 'function') haptic('light');
    });
    actions.appendChild(replyBtn);

    var copyBtn = document.createElement('button');
    copyBtn.className = 'msg-ctx-btn msg-ctx-copy';
    copyBtn.type = 'button';
    copyBtn.innerHTML = _messageActionIcon('copy') + '<span>Copy</span>';
    copyBtn.addEventListener('click', function(e) {
        e.stopPropagation();
        _dismissContextMenu();
        _copyToClipboard(msgData.content || '').then(function(ok) {
            if (typeof showToast === 'function') {
                showToast(ok ? 'Message copied' : 'Could not copy', ok ? 'toast-green' : 'toast-orange', 1600);
            }
            if (typeof haptic === 'function') haptic(ok ? 'success' : 'warning');
        });
    });
    actions.appendChild(copyBtn);

    menu.appendChild(actions);

    document.body.appendChild(menu);
    _activeContextMenu = { menu: menu, row: row, container: container };
    _positionMsgContextMenu(menu, x, y, bubble);
}

function _handleMessageActionPointer(e) {
    if (!_activeContextMenu) return;
    var menu = _activeContextMenu.menu;
    var row = _activeContextMenu.row;
    if ((menu && menu.contains(e.target)) || (row && row.contains(e.target))) return;
    _dismissContextMenu();
}

document.addEventListener('pointerdown', _handleMessageActionPointer, true);
document.addEventListener('mousedown', _handleMessageActionPointer, true);
document.addEventListener('keydown', function(e) {
    if (e.key === 'Escape') _dismissContextMenu();
}, true);

window.RS = window.RS || {};
window.RS.closeMessageActionMenu = function() {
    if (_activeContextMenu) {
        _dismissContextMenu();
        return true;
    }
    return false;
};

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

function addLxmfContact() {
    var hashInput = document.getElementById('lxmf-add-hash');
    var nameInput = document.getElementById('lxmf-add-name');
    var hash = hashInput.value.trim();
    var name = nameInput.value.trim();

    if (!hash || hash.length < 16) {
        showPreConditionToast('Enter a valid identity hash (at least 16 hex chars)');
        return;
    }

    if (!/^[0-9a-fA-F]+$/.test(hash)) {
        showPreConditionToast('Hash must contain only hex characters (0-9, a-f)');
        return;
    }

    if (hash.length > 64) {
        showPreConditionToast('Hash is too long (maximum 64 characters)');
        return;
    }

    RS.invoke('add_contact', {
        args: {
            hash: hash,
            display_name: name || null,
        }
    }).catch(function() {});

    hashInput.value = '';
    nameInput.value = '';
}

RS.listen('lxmf_identity', function(data) {
    lxmfIdentity = data;
    var el = document.getElementById('lxmf-own-hash');
    if (el && data.hash) {
        el.innerHTML = copyableHash(data.hash, 16);
    }
    var avatarEl = document.getElementById('msg-profile-avatar');
    var nameEl = document.getElementById('msg-profile-name');
    if (avatarEl && data.hash) {
        avatarEl.innerHTML = identityAvatar(data.hash, 36);
    }
    if (nameEl && data.display_name) {
        nameEl.textContent = data.display_name;
    }
    // Re-render so we exclude self from the peer list.
    if (typeof renderConnectionsTable === 'function' && typeof PeersCache !== 'undefined' && PeersCache) {
        renderConnectionsTable(PeersCache.enriched());
    }
});

// Pre-warmed at startup; re-emitted on every conversation-touching command.
RS.listen('conversations_update', function(data) {
    lxmfConversations = Array.isArray(data) ? data : [];
    _conversationsFirstLoadDone = true;
    _renderConversationsFromCache(lxmfConversations);
});

if (typeof PeersCache !== 'undefined' && PeersCache && typeof PeersCache.subscribe === 'function') {
    PeersCache.subscribe(function() {
        _refreshRenderedConversationNames();
        renderVoiceUi();
    });
}

RS.listen('contacts_update', function(data) {
    lxmfContacts = normalizeContactList(data);
    // peer_updated emissions handle PeersCache; no optimistic patch needed.
    renderContactList();
    if (typeof renderStandaloneContactList === 'function') renderStandaloneContactList();
    _refreshRenderedConversationNames();
    renderVoiceUi();
});

RS.listen('contact_blocked', function(data) {
    if (typeof showToast === 'function') showToast('User blocked', 'toast-green', 2000);
    // Optimistic removal; idempotent peer_removed event will follow.
    if (data && data.hash && typeof PeersCache !== 'undefined' && PeersCache) {
        PeersCache.applyRemoved(data.hash);
    }
    if (typeof refreshPeersList === 'function') refreshPeersList();
});

RS.listen('contact_unblocked', function(data) {
    if (typeof showToast === 'function') showToast('User unblocked', 'toast-green', 2000);
    if (typeof refreshPeersList === 'function') refreshPeersList();
});

RS.listen('conversation_update', function(data) {
    cacheSet(data.hash, data.messages || []);
    if (data.hash === lxmfActiveContact) {
        lxmfConversation = data.messages || [];
        renderConversation({ stickToBottom: true });
    }
});

RS.listen('lxmf_message', function(msg) {
    if (msg.source === lxmfActiveContact || msg.destination === lxmfActiveContact) {
        // Dedupe reconnect replays.
        var isDupe = msg.id && lxmfConversation.some(function(m) { return m.id === msg.id; });
        if (isDupe) return;
        lxmfConversation.push(msg);
        cacheSet(lxmfActiveContact, lxmfConversation.slice());
        renderConversation({ stickToBottom: true });
        if (msg.source === lxmfActiveContact) {
            RS.invoke('mark_read', { hash: msg.source }).catch(function() {});
        }
    }
    if (msg.source !== lxmfActiveContact) {
        var fromLabel = _messageSourceName(msg);
        var hasAttachment = (msg.attachments && msg.attachments.length > 0) || msg.image;
        var toastMsg = hasAttachment
            ? 'New message with attachment from ' + escapeHtml(fromLabel)
            : 'New message from ' + escapeHtml(fromLabel);
        var sourceHash = msg.source;
        showToast(toastMsg, 'toast-blue', 4000, function() { openConversationWith(sourceHash); });
        if (!window.__TAURI_INTERNALS__ && document.hidden && typeof rsNotify !== 'undefined') {
            var notifFrom = _messageSourceName(msg);
            var notifBody = (msg.content || '').substring(0, 120) || 'New message';
            rsNotify.send({
                title: 'Message from ' + notifFrom,
                body: notifBody,
                tag: 'lxmf-' + msg.source,
                onClick: function() { openConversationWith(msg.source); }
            });
        }
    }
});

RS.listen('lxmf_step', function(data) {
    // Remap optimistic client_msg_id to canonical server msg_id.
    if (data.client_msg_id && data.msg_id) {
        for (var ri = 0; ri < lxmfConversation.length; ri++) {
            if (lxmfConversation[ri].id === data.client_msg_id) {
                lxmfConversation[ri].id = data.msg_id;
                break;
            }
        }
        _flushPendingLxmfCancel(data.client_msg_id, data.msg_id);
    }
    if (data.step === 'delivered' || data.step === 'propagated' || data.step === 'failed' || data.step === 'cancelled' || data.step === 'timeout' || data.step === 'error' || data.step === 'rejected') {
        var resolvedState = (data.step === 'error') ? 'failed' : data.step;
        // `propagated` is terminal alongside `delivered`/`failed`/`cancelled`/
        // `rejected` — see db::update_message_state for the matching guard.
        var terminalStates = ['delivered', 'propagated', 'failed', 'cancelled', 'rejected'];
        var matched = false;
        lxmfConversation.forEach(function(msg) {
            if (data.msg_id && msg.id === data.msg_id) {
                if (terminalStates.indexOf(msg.state) === -1) {
                    msg.state = resolvedState;
                    if (data.rtt_ms) msg.rtt = data.rtt_ms;
                    if (data.method) msg.delivery_method = data.method;
                }
                matched = true;
            }
        });
        // Fallback for legacy events with no msg_id.
        if (!matched && !data.msg_id) {
            for (var i = lxmfConversation.length - 1; i >= 0; i--) {
                var msg = lxmfConversation[i];
                if (msg.state === 'sending' || msg.state === 'sent') {
                    msg.state = resolvedState;
                    break;
                }
            }
        }
        renderConversation();
    }
    var inFlightSteps = [
        'sent',
        'routing',
        'propagating',
        'resolving',
        'link_establishing',
        'sending_via_link',
        'reusing_direct_link',
        'reusing_backchannel'
    ];
    if (inFlightSteps.indexOf(data.step) !== -1) {
        lxmfConversation.forEach(function(msg) {
            if (data.msg_id && msg.id === data.msg_id) {
                if (!_isTerminalMessageState(msg.state)) msg.state = data.step;
                if (data.method) msg.delivery_method = data.method;
            }
        });
        renderConversation();
    }

    if (data.step === 'timeout') {
        showToast('Message timed out: destination may be unreachable', 'toast-red', 5000);
    }
    if (data.step === 'error') {
        showToast(data.message || 'Send error', 'toast-red', 5000);
    }
    if (data.step === 'rejected') {
        showToast(data.message || 'Message rejected by destination', 'toast-red', 5000);
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
    var inFlightSteps = [
        'routing',
        'propagating',
        'link_establishing',
        'sending_via_link',
        'reusing_direct_link',
        'reusing_backchannel'
    ];
    var terminalStates = ['delivered', 'propagated', 'failed', 'cancelled', 'rejected'];
    var changed = false;
    lxmfConversation.forEach(function(msg) {
        if (data.msg_id && msg.id === data.msg_id) {
            if (typeof data.progress === 'number') msg.delivery_progress = data.progress;
            if (data.link_id) msg.delivery_link_id = data.link_id;
            if (data.representation) msg.delivery_representation = data.representation;
            if (data.method) msg.delivery_method = data.method;
            if (data.step && inFlightSteps.indexOf(data.step) !== -1 && terminalStates.indexOf(msg.state) === -1) {
                msg.state = data.step;
            }
            changed = true;
        }
    });
    if (changed) renderConversation();
});

RS.listen('voice_call_update', _voiceHandleUpdate);
RS.listen('voice_incoming_call', function(data) {
    _voiceHandleUpdate(Object.assign({ type: 'incoming' }, data || {}));
});

document.addEventListener('rs-audio-playback-ready', _voiceSyncRingtone);
document.addEventListener('visibilitychange', function() {
    if (!document.hidden) _voiceSyncRingtone();
});

RS.listen('contact_added', function(data) {
    showToast('Contact added: ' + data.display_name, 'toast-green', 3000);
    lxmfActiveContact = data.hash;
    renderContactList();
    if (typeof renderStandaloneContactList === 'function') renderStandaloneContactList();
    RS.invoke('get_conversation', { hash: data.hash }).catch(function() {});
    if (typeof refreshPeersList === 'function') refreshPeersList();
});

RS.listen('contact_error', function(data) {
    showToast(data.error || 'Contact operation failed', 'toast-red', 4000);
});

function _aboutClassifyIface(iface) {
    if (!iface) return null;
    var s = String(iface).toLowerCase();
    if (s.indexOf('rnode') >= 0) return 'LoRa';
    if (s.indexOf('ble_peer') >= 0 || s.indexOf('ble mesh') >= 0 || s.indexOf('bluetooth peer') >= 0) return 'Bluetooth Peer';
    if (s.indexOf('ble') >= 0) return 'Bluetooth';
    if (s.indexOf('androidusb') >= 0) return 'USB';
    if (s.indexOf('serial') >= 0) return 'Serial';
    if (s.indexOf('udp') >= 0) return 'UDP';
    if (s.indexOf('tcp') >= 0 || /:\d+$/.test(s)) return 'TCP';
    if (s.indexOf('i2p') >= 0) return 'I2P';
    if (s.indexOf('auto') >= 0) return 'Auto';
    if (s.indexOf('local') >= 0) return 'Local';
    return iface.charAt(0).toUpperCase() + iface.slice(1);
}

function _aboutChipColor(status) {
    if (status === 'reachable' || status === 'direct') return 'green';
    if (status === 'stale') return 'warning';
    return 'muted';
}

function showContactAbout(hash) {
    var reach = null;
    var _peers = _lxmfPeers();
    for (var i = 0; i < _peers.length; i++) {
        if (_peers[i].hash === hash) { reach = _peers[i]; break; }
    }
    var name = _conversationNameInfo(hash, null, false).name;
    var status = reach ? reach.status : 'unknown';
    var chipColor = _aboutChipColor(status);
    var avatarStateClass = (status === 'reachable' || status === 'direct') ? 'online' : (status === 'stale' ? 'stale' : '');
    var activityLabel = _peerActivityLabel(reach);
    var routeLabel = _peerRouteLabel(reach);

    var hops = (reach && reach.hops !== null && reach.hops !== undefined) ? reach.hops : '\u2014';
    var pathAge = (reach && reach.in_path && reach.path_age !== null && reach.path_age !== undefined) ? prettyTime(reach.path_age) + ' ago' : '\u2014';
    var idKnown = reach ? (reach.identity_known ? 'Yes' : 'No') : 'Unknown';
    var msgCount = lxmfConversation ? lxmfConversation.length : 0;

    var ifaceLabel = _aboutClassifyIface(reach ? reach.iface : null);
    var ifaceLive = !!(reach && reach.iface_is_live);

    var nowSecs = Date.now() / 1000;
    var firstSeen = (reach && reach.first_seen) ? reach.first_seen : null;
    var firstSeenText = firstSeen ? (typeof formatLastHeard === 'function' ? formatLastHeard(firstSeen) : prettyTime(nowSecs - firstSeen) + ' ago') : '\u2014';
    var firstSeenTitle = firstSeen ? new Date(firstSeen * 1000).toLocaleString() : '';
    var lastHeard = (reach && reach.last_seen) ? reach.last_seen : null;
    var lastHeardText = typeof formatLastHeard === 'function' ? formatLastHeard(lastHeard) : (lastHeard ? prettyTime(nowSecs - lastHeard) + ' ago' : 'No activity yet');
    var lastHeardTitle = lastHeard ? new Date(lastHeard * 1000).toLocaleString() : '';

    var viaHash = (reach && reach.in_path) ? reach.via : null;
    var viaIsRelay = !!(viaHash && viaHash !== hash && reach && (reach.hops == null || reach.hops > 1));
    var viaShort = viaHash ? (typeof shortHash === 'function' ? shortHash(viaHash, 6, 4) : viaHash.substring(0, 6) + '\u2026' + viaHash.substring(viaHash.length - 4)) : '';
    var viaInPeerCache = false;
    if (viaIsRelay) {
        for (var j = 0; j < _peers.length; j++) {
            if (_peers[j].hash === viaHash) { viaInPeerCache = true; break; }
        }
    }

    var idBadgeClass = (idKnown === 'Yes') ? 'about-id-badge about-id-badge-verified' : 'about-id-badge about-id-badge-unverified';
    var idBadgeText = (idKnown === 'Yes') ? 'Verified' : 'Unverified';

    var modal = document.getElementById('node-modal');
    if (!modal) return;
    document.getElementById('modal-title').textContent = 'About';
    var body = modal.querySelector('.bottom-sheet-body');

    var html = '';

    html += '<div class="about-hero">';
    html += '  <div class="about-hero-avatar ' + avatarStateClass + '">' + identityAvatar(hash, 56) + '</div>';
    html += '  <div class="about-hero-name">' + escapeHtml(name) + '</div>';
    html += '  <div class="about-hero-address" title="' + escapeHtml(hash) + '">' + copyableHash(hash, 8) + '</div>';
    html += '  <span class="' + idBadgeClass + '">' + idBadgeText + '</span>';
    html += '</div>';

    html += '<div class="about-section-label">Availability</div>';
    html += '<div class="about-section">';
    html += '  <div class="about-row">';
    html += '    <span class="about-row-label">Activity</span>';
    html += '    <span class="about-status-chip about-status-chip-' + chipColor + '">';
    html += '      <span class="dot ' + (chipColor === 'muted' ? 'gray' : (chipColor === 'warning' ? 'orange' : 'green')) + '"></span>';
    html += escapeHtml(activityLabel);
    html += '    </span>';
    html += '  </div>';
    html += '  <div class="about-row">';
    html += '    <span class="about-row-label">Route</span>';
    html += '    <span class="about-row-value">' + escapeHtml(routeLabel) + '</span>';
    html += '  </div>';
    html += '  <div class="about-row about-row-pair">';
    html += '    <span class="about-pair-cell"><span class="about-row-label">Hops</span><span class="about-row-value">' + hops + '</span></span>';
    html += '    <span class="about-pair-cell"><span class="about-row-label">Path Age</span><span class="about-row-value">' + escapeHtml(pathAge) + '</span></span>';
    html += '  </div>';
    if (ifaceLabel) {
        html += '  <div class="about-row">';
        html += '    <span class="about-row-label">Interface</span>';
        html += '    <span class="about-row-value">' + escapeHtml(ifaceLabel) + (ifaceLive ? '' : ' <span class="about-iface-stale">\u00b7 last known</span>') + '</span>';
        html += '  </div>';
    }
    html += '  <div class="about-row">';
    html += '    <span class="about-row-label">Via</span>';
    if (viaIsRelay) {
        html += '    <a href="#" class="about-via-link" data-via="' + escapeHtml(viaHash) + '" data-known="' + (viaInPeerCache ? '1' : '0') + '">' + escapeHtml(viaShort) + ' \u2192</a>';
    } else if (reach && reach.in_path) {
        html += '    <span class="about-row-value about-row-value-muted">Direct</span>';
    } else {
        html += '    <span class="about-row-value about-row-value-muted">No current path</span>';
    }
    html += '  </div>';
    html += '</div>';

    html += '<div class="about-section-label">Activity</div>';
    html += '<div class="about-section">';
    html += '  <div class="about-row">';
    html += '    <span class="about-row-label">Last heard</span>';
    html += '    <span class="about-row-value"' + (lastHeardTitle ? ' title="' + escapeHtml(lastHeardTitle) + '"' : '') + '>' + escapeHtml(lastHeardText) + '</span>';
    html += '  </div>';
    html += '  <div class="about-row">';
    html += '    <span class="about-row-label">First heard</span>';
    html += '    <span class="about-row-value"' + (firstSeenTitle ? ' title="' + escapeHtml(firstSeenTitle) + '"' : '') + '>' + escapeHtml(firstSeenText) + '</span>';
    html += '  </div>';
    html += '  <div class="about-row">';
    html += '    <span class="about-row-label">Messages</span>';
    if (msgCount > 0) {
        html += '    <a href="#" class="about-messages-link">' + msgCount + ' \u00b7 View \u2192</a>';
    } else {
        html += '    <span class="about-row-value about-row-value-muted">0</span>';
    }
    html += '  </div>';
    html += '</div>';

    body.innerHTML = html;
    var overlay = document.getElementById('node-modal-overlay');
    modal.classList.add('open');
    if (overlay) overlay.classList.add('active');

    // CSP blocks inline onclick — wire here.
    var viaLink = body.querySelector('.about-via-link');
    if (viaLink) {
        viaLink.addEventListener('click', function(ev) {
            ev.preventDefault();
            ev.stopPropagation();
            var v = viaLink.getAttribute('data-via');
            if (viaLink.getAttribute('data-known') === '1') {
                showContactAbout(v);
            } else {
                showToast('Relay not in peer list', 'toast-orange', 1500);
            }
        });
    }
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
}

function openConversationWith(hash) {
    if (_ghostConversationHash && _ghostConversationHash !== hash) {
        _removeGhostRow();
    }
    var input = document.getElementById('lxmf-input');
    if (input && lxmfActiveContact) {
        if (input.value.trim()) { _lxmfDrafts[lxmfActiveContact] = input.value; }
        else { delete _lxmfDrafts[lxmfActiveContact]; }
    }
    if (typeof switchView === 'function') switchView('message');
    lxmfActiveContact = hash;
    if (input) { input.value = _lxmfDrafts[hash] || ''; input.style.height = ''; }
    _loadConversation(hash);
    _ensureGhostRow(hash);
    var input = document.getElementById('lxmf-input');
    if (input) input.focus();
    if (window.innerWidth <= 768) {
        RS.viewStack.push('chat-detail', { meta: { contactHash: hash } });
        history.pushState({ view: 'message', detail: true }, '', '#message');
    }
}

function initFabSpeedDial() {
    if (!isMobile()) return;
    var mainFab = document.getElementById('lxmf-send-fab');
    var dialActions = document.getElementById('fab-dial-actions');
    if (!mainFab || !dialActions) return;

    var scrim = document.createElement('div');
    scrim.className = 'fab-dial-scrim';
    document.body.appendChild(scrim);

    var _dialOpen = false;

    function openDial() {
        _dialOpen = true;
        mainFab.classList.add('dial-open');
        mainFab.setAttribute('aria-expanded', 'true');
        dialActions.classList.add('open');
        scrim.classList.add('active');
    }

    function closeDial() {
        _dialOpen = false;
        mainFab.classList.remove('dial-open');
        mainFab.setAttribute('aria-expanded', 'false');
        dialActions.classList.remove('open');
        scrim.classList.remove('active');
    }

    // Exposed so nav.js can close the dial on view switch.
    window._closeFabDial = closeDial;

    RS.gestures.bindViewFabClick(mainFab, function(e) {
        e.stopPropagation();
        _dialOpen ? closeDial() : openDial();
    });

    scrim.addEventListener('click', closeDial);

    var dialNew = document.getElementById('fab-dial-new');
    if (dialNew) {
        dialNew.addEventListener('click', function() {
            if (typeof haptic === 'function') haptic('selection');
            closeDial();
            var btn = document.getElementById('lxmf-send-message-btn');
            if (btn) btn.click();
        });
    }

    var dialContacts = document.getElementById('fab-dial-contacts');
    if (dialContacts) {
        dialContacts.addEventListener('click', function() {
            if (typeof haptic === 'function') haptic('selection');
            closeDial();
            openFabContactPicker();
        });
    }
}

function openFabContactPicker() {
    var sheet = document.getElementById('fab-contact-picker-sheet');
    var overlay = document.getElementById('fab-contact-picker-overlay');
    var listEl = document.getElementById('fab-contact-picker-list');
    if (!sheet || !overlay || !listEl) return;

    if (!lxmfContacts || lxmfContacts.length === 0) {
        listEl.innerHTML = '<div class="fab-picker-empty">No contacts yet.<br>Add a contact first to message them here.</div>';
    } else {
        var sorted = lxmfContacts.slice().sort(function(a, b) {
            var na = (a.display_name || '').toLowerCase();
            var nb = (b.display_name || '').toLowerCase();
            return na < nb ? -1 : na > nb ? 1 : 0;
        });
        var html = '';
        sorted.forEach(function(c) {
            var name = c.display_name || 'Anonymous';
            var avatarHtml = '<span style="width:32px;height:32px;flex-shrink:0;display:flex;">' + identityAvatar(c.hash, 32) + '</span>';
            html += '<div class="fab-picker-row" data-hash="' + escapeHtml(c.hash) + '">' +
                avatarHtml +
                '<div>' +
                    '<div class="fab-picker-name">' + ratspeakDisplayNameHtml(name, c) + '</div>' +
                    '<div class="fab-picker-hash">' + escapeHtml(typeof shortHash === 'function' ? shortHash(c.hash, 8, 4) : c.hash.substring(0, 12) + '…') + '</div>' +
                '</div>' +
            '</div>';
        });
        listEl.innerHTML = html;
        listEl.querySelectorAll('.fab-picker-row').forEach(function(row) {
            row.addEventListener('click', function() {
                closeFabContactPicker();
                openConversationWith(this.dataset.hash);
            });
        });
    }

    overlay.classList.add('active');
    sheet.classList.add('open');
    overlay.onclick = function() { closeFabContactPicker(); };
    history.pushState({ view: currentView, fabPicker: true }, '', '#' + currentView);
}

function closeFabContactPicker() {
    var sheet = document.getElementById('fab-contact-picker-sheet');
    var overlay = document.getElementById('fab-contact-picker-overlay');
    if (sheet) sheet.classList.remove('open');
    if (overlay) overlay.classList.remove('active');
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
        if (files.length > 1) showToast('Only one file can be attached at a time', 'toast-orange', 3000);
        handleFileSelected({ files: [files[0]], value: '' });
    });
})();

(function() {
    var chatArea = document.getElementById('lxmf-chat-area');
    if (!chatArea) return;
    chatArea.addEventListener('paste', function(e) {
        var items = e.clipboardData && e.clipboardData.items;
        if (!items) return;
        var file = null;
        for (var i = 0; i < items.length; i++) {
            if (items[i].kind === 'file') {
                file = items[i].getAsFile();
                break;
            }
        }
        if (!file) return;
        e.preventDefault();
        if (!lxmfActiveContact) { showPreConditionToast('Select a conversation first'); return; }
        // Rename pasted 'image.png' with a timestamp to avoid backend collisions.
        if (file.name === 'image.png' || !file.name) {
            var ts = new Date().toISOString().replace(/[:.]/g, '-').slice(0, 19);
            var ext = (file.type && file.type.split('/')[1]) || 'png';
            file = new File([file], 'paste-' + ts + '.' + ext, { type: file.type });
        }
        handleFileSelected({ files: [file], value: '' });
    });
})();

function openChatHeaderDropdown(triggerEl) {
    if (!lxmfActiveContact) return;

    var chatHeader = document.getElementById('lxmf-chat-header');
    if (!chatHeader) return;

    var contact = lxmfContacts.find(function(c) { return c.hash === lxmfActiveContact; });
    var menuTrigger = triggerEl || document.getElementById('chat-header-menu-btn') || chatHeader;
    var currentName = contact ? contact.display_name : '';
    var items = [];

    if (lxstVoiceState.available) {
        var callInOtherConversation = !!(lxstVoiceState.active && !_voiceActiveMatchesContact());
        items.push({
            label: callInOtherConversation ? 'Call in Progress' : _voicePrimaryActionLabel(),
            icon: callInOtherConversation ? _voiceIcon('phone', 18) : _voicePrimaryActionIcon(),
            danger: _voiceActiveMatchesContact(),
            disabled: callInOtherConversation,
            onSelect: function() {
                _voiceBlockMobileNavigation(280);
                _voiceRunPrimaryAction(lxmfActiveContact);
            }
        });
    }

    items.push(
        {
            label: contact ? 'Rename Contact' : 'Add Contact',
            icon: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 20h9"/><path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4Z"/></svg>',
            onSelect: function() {
                rsPrompt({ message: contact ? 'New name:' : 'Contact name:', defaultValue: currentName || '', placeholder: 'Display name' }).then(function(newName) {
                    if (newName !== null) {
                        RS.invoke('add_contact', { args: { hash: lxmfActiveContact, display_name: newName.trim() || null } }).catch(function() {});
                    }
                });
            }
        },
        {
            label: 'About',
            icon: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><path d="M12 16v-4"/><path d="M12 8h.01"/></svg>',
            onSelect: function() { showContactAbout(lxmfActiveContact); }
        },
        {
            label: 'Copy LXMF Hash',
            icon: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>',
            onSelect: function() {
                if (!lxmfActiveContact) return;
                navigator.clipboard.writeText(lxmfActiveContact).then(function() {
                    showCopyConfirmationToast('Hash');
                }).catch(function() {
                    showToast('Could not copy', 'toast-orange', 1500);
                });
            }
        },
        {
            label: 'Delete Conversation',
            danger: true,
            icon: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M3 6h18"/><path d="M8 6V4h8v2"/><path d="M19 6l-1 14H6L5 6"/></svg>',
            onSelect: function() {
                var name = _conversationNameInfo(lxmfActiveContact, null, false).name;
                showConversationDeleteDialog(lxmfActiveContact, name);
            }
        }
    );

    if (RS.ui && typeof RS.ui.openActionMenu === 'function') {
        RS.ui.openActionMenu(menuTrigger, items, { title: 'Conversation' });
    } else if (typeof actionPopover === 'function') {
        actionPopover(menuTrigger, items);
    }
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
        renderVoiceUi();
    }).catch(function() {
        lxstVoiceState.available = false;
        renderVoiceUi();
    });

    var msgContainer = document.getElementById('lxmf-messages');
    if (msgContainer) _wireLxmfMessageScroll(msgContainer);
    if (msgContainer && typeof isMobile === 'function' && isMobile()) {
        msgContainer.addEventListener('touchstart', function(e) {
            var t = e.target;
            // Don't blur for taps on the compose bar or interactive controls — only
            // for taps on message body / list whitespace.
            if (t && t.closest && (
                t.closest('.lxmf-compose') ||
                t.closest('button, a, input, textarea, select, [role="button"], [role="menuitem"]')
            )) return;
            var active = document.activeElement;
            if (active && (active.tagName === 'INPUT' || active.tagName === 'TEXTAREA')) {
                active.blur();
            }
        }, { passive: true });
    }

    var addBtn = document.getElementById('add-contact-btn');
    if (addBtn) addBtn.addEventListener('click', function() {
        addLxmfContact();
        var form = document.getElementById('add-contact-form');
        if (form) form.style.display = 'none';
    });

    ['lxmf-add-hash', 'lxmf-add-name'].forEach(function(id) {
        var el = document.getElementById(id);
        if (el) el.addEventListener('keydown', function(e) {
            if (e.key === 'Enter') { e.preventDefault(); addLxmfContact(); }
        });
    });

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

    var ICON_EMOJI = '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><path d="M8 14s1.5 2 4 2 4-2 4-2"/><line x1="9" y1="9" x2="9.01" y2="9"/><line x1="15" y1="9" x2="15.01" y2="9"/></svg>';
    var ICON_FILE = '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/></svg>';
    var ICON_CAMERA = '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M23 19a2 2 0 0 1-2 2H3a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h4l2-3h6l2 3h4a2 2 0 0 1 2 2z"/><circle cx="12" cy="13" r="4"/></svg>';
    var ICON_VIDEO = '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m22 8-6 4 6 4V8Z"/><rect x="2" y="6" width="14" height="12" rx="2"/></svg>';
    var ICON_PHOTOS = '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="3" width="18" height="18" rx="2" ry="2"/><circle cx="8.5" cy="8.5" r="1.5"/><polyline points="21 15 16 10 5 21"/></svg>';
    var ICON_CONTACTS = '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/><path d="M23 21v-2a4 4 0 0 0-3-3.87"/><path d="M16 3.13a4 4 0 0 1 0 7.75"/></svg>';
    var ICON_NEW = '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 20h9"/><path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4Z"/></svg>';
    var ICON_ROUTE = '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="6" cy="18" r="2"/><circle cx="18" cy="6" r="2"/><path d="M8 17c5-2 7-4 9-9"/></svg>';
    var ICON_RELAY = '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="5" width="18" height="14" rx="2"/><path d="M3 7l9 6 9-6"/></svg>';

    function openSendMethodPopover() {
        if (!sendBtn || typeof actionPopover !== 'function' || !lxmfActiveContact) return;
        var hasRelay = false;
        if (typeof propagationStatus !== 'undefined' && propagationStatus.mode !== 'off') {
            hasRelay = propagationStatus.mode === 'auto'
                ? !!propagationStatus.auto_active_node
                : !!propagationStatus.propagation_node;
        }
        var input = document.getElementById('lxmf-input');
        var wasFocused = !!(input && document.activeElement === input);
        actionPopover(sendBtn, [
            { label: 'Opportunistic', icon: ICON_ROUTE, onSelect: function() { sendLxmfMessage('opportunistic'); } },
            { label: 'Direct', icon: ICON_ROUTE, onSelect: function() { sendLxmfMessage('direct'); } },
            { label: 'Offline Inbox', icon: ICON_RELAY, disabled: !hasRelay, onSelect: function() { sendLxmfMessage('propagated'); } },
        ], {
            onClose: function() {
                if (wasFocused && input && document.activeElement !== input) {
                    _focusLxmfComposerInput(input);
                }
            }
        });
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
            // iOS gets the native action sheet for bare <input type="file">.
            // Android adds a Camera/Photos/File popover (WebChromeClient lacks camera).
            // Desktop adds an Emoji entry since the OS keyboard doesn't help there.
            if (typeof actionPopover !== 'function') {
                triggerFileAttachment();
                return;
            }
            if (isIOS()) {
                triggerFileAttachment();
                return;
            }
            if (isMobile()) {
                actionPopover(attachBtn, [
                    { label: 'Camera', icon: ICON_CAMERA, onSelect: triggerCameraAttachment },
                    { label: 'Video', icon: ICON_VIDEO, onSelect: triggerVideoAttachment },
                    { label: 'Photos', icon: ICON_PHOTOS, onSelect: triggerPhotosAttachment },
                    { label: 'File', icon: ICON_FILE, onSelect: triggerFileAttachment },
                ]);
                return;
            }
            actionPopover(attachBtn, [
                { label: 'Emoji', icon: ICON_EMOJI, onSelect: openComposeEmojiPicker },
                { label: 'File / Image', icon: ICON_FILE, onSelect: triggerFileAttachment },
            ]);
        });
    }

    function promptNewConversationHash() {
        rsPrompt({ title: 'New Conversation', message: 'Enter identity hash:', placeholder: '16+ hex characters' }).then(function(hash) {
            if (hash === null) return;
            hash = hash.trim();
            if (hash.length >= 16 && /^[0-9a-fA-F]+$/.test(hash)) {
                openConversationWith(hash);
            } else {
                showPreConditionToast('Enter a valid identity hash (at least 16 hex chars)');
            }
        });
    }

    // Mobile FAB speed-dial calls .click() here for direct hash-prompt;
    // desktop gets a Contacts/New popover instead.
    var sendMsgBtn = document.getElementById('lxmf-send-message-btn');
    if (sendMsgBtn) {
        sendMsgBtn.addEventListener('click', function(e) {
            if (isMobile() || typeof actionPopover !== 'function') {
                promptNewConversationHash();
                return;
            }
            e.stopPropagation();
            actionPopover(sendMsgBtn, [
                { label: 'Contacts', icon: ICON_CONTACTS, onSelect: function() {
                    if (typeof openFabContactPicker === 'function') openFabContactPicker();
                } },
                { label: 'New',      icon: ICON_NEW,      onSelect: promptNewConversationHash }
            ]);
        });
    }

    initFabSpeedDial();

    var addContactTabBtn = document.getElementById('lxmf-add-contact-btn');
    if (addContactTabBtn) {
        addContactTabBtn.addEventListener('click', function() {
            var form = document.getElementById('add-contact-form');
            if (form) {
                var visible = form.style.display !== 'none';
                form.style.display = visible ? 'none' : 'flex';
                if (!visible) {
                    var hashInput = document.getElementById('lxmf-add-hash');
                    if (hashInput && !isMobile()) hashInput.focus();
                }
            }
        });
    }

    var chatAddBtn = document.getElementById('lxmf-chat-add-contact-btn');
    if (chatAddBtn) {
        chatAddBtn.addEventListener('click', function(e) {
            e.stopPropagation();
            if (!lxmfActiveContact) return;
            rsPrompt({ message: 'Contact name (optional):', placeholder: 'Display name' }).then(function(name) {
                if (name === null) return;
                RS.invoke('add_contact', { args: { hash: lxmfActiveContact, display_name: name.trim() || null } }).catch(function() {});
            });
        });
    }

    // Mobile: Enter always inserts newline (avoid OSK return-key accidental sends).
    // Desktop: Enter sends, Shift+Enter inserts newline.
    var textarea = document.getElementById('lxmf-input');
    if (textarea) {
        textarea.removeAttribute('maxlength');
        textarea.addEventListener('keydown', function(e) {
            if (e.key === 'Enter' && !e.shiftKey && !isMobile()) {
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
                if (byteLen > efficientBytes) {
                    counter.style.display = '';
                    counter.textContent = prettySize(byteLen) + ' / ' + prettySize(maxBytes);
                    counter.className = 'lxmf-char-count' +
                        (byteLen > maxBytes ? ' char-limit' : ' char-warn');
                } else {
                    counter.style.display = 'none';
                }
            }
        });
    }

    var trigger = document.getElementById('add-contact-trigger');
    if (trigger) {
        trigger.addEventListener('click', function() {
            var form = document.getElementById('add-contact-form');
            if (form) {
                var visible = form.style.display !== 'none';
                form.style.display = visible ? 'none' : 'flex';
                if (!visible) {
                    var hashInput = document.getElementById('lxmf-add-hash');
                    if (hashInput && !isMobile()) hashInput.focus();
                }
            }
        });
    }

    var cancelBtn = document.getElementById('add-contact-cancel');
    if (cancelBtn) {
        cancelBtn.addEventListener('click', function() {
            var form = document.getElementById('add-contact-form');
            if (form) form.style.display = 'none';
        });
    }

    var chatHeaderMenuBtn = document.getElementById('chat-header-menu-btn');
    var chatHeader = document.getElementById('lxmf-chat-header');
    if (chatHeaderMenuBtn) {
        chatHeaderMenuBtn.addEventListener('click', function(e) {
            e.stopPropagation();
            openChatHeaderDropdown(e.currentTarget);
        });
    }

    // Identity area is tappable, not just the kebab.
    var contactAvatar = document.getElementById('lxmf-contact-avatar');
    if (contactAvatar) {
        contactAvatar.addEventListener('click', function(e) {
            e.stopPropagation();
            openChatHeaderDropdown(e.currentTarget);
        });
    }
    var headerInfo = chatHeader ? chatHeader.querySelector('.lxmf-chat-header-info') : null;
    if (headerInfo) {
        headerInfo.addEventListener('click', function(e) {
            e.stopPropagation();
            openChatHeaderDropdown(e.currentTarget);
        });
    }

    if (lxmfContacts.length > 0) {
        RS.invoke('check_contact_status').catch(function() {});
    }

    var lxmfBackBtn = document.getElementById('lxmf-back-btn');
    if (lxmfBackBtn) {
        lxmfBackBtn.addEventListener('click', function(e) {
            e.stopPropagation();
            RS.viewStack.pop();
        });
    }

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

RS.listen('contact_identity_status', function(data) {
    contactIdentityStatus = data;
    renderContactList();
    if (typeof renderStandaloneContactList === 'function') renderStandaloneContactList();
});

RS.listen('unread_total', function(data) {
    var dot = document.getElementById('nav-unread-dot');
    if (dot) dot.style.display = (data.count > 0) ? '' : 'none';
    var bbDot = document.getElementById('bb-unread');
    if (bbDot) bbDot.style.display = (data.count > 0) ? '' : 'none';
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
    cacheDel(data.hash);
    if (lxmfActiveContact === data.hash) {
        lxmfActiveContact = null;
        lxmfConversation = [];
        renderConversation();
    }
    if (_ghostConversationHash === data.hash) _removeGhostRow();
    loadConversations();
});

RS.listen('conversation_deleted', function(data) {
    if (!data.ok) return;
    cacheDel(data.hash);
    if (lxmfActiveContact === data.hash) {
        lxmfActiveContact = null;
        lxmfConversation = [];
        renderConversation();
    }
    if (_ghostConversationHash === data.hash) _removeGhostRow();
    loadConversations();
    showToast('Conversation deleted', 'toast-green', 3000);
});

// 30s re-check so identity/path changes surface without a page reload.
setInterval(function() {
    var view = document.getElementById('view-message');
    if (view && view.style.display !== 'none' && lxmfContacts.length > 0) {
        RS.invoke('check_contact_status').catch(function() {});
    }
}, 30000);

function _imageActionIcon(name) {
    var icons = {
        close: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round"><line x1="6" y1="6" x2="18" y2="18"/><line x1="18" y1="6" x2="6" y2="18"/></svg>',
        copy: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>',
        save: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/></svg>'
    };
    return icons[name] || '';
}

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
        filename: resolvedName,
        mime: resolvedMime,
        data_base64: match[2]
    };
}

function _fileFromImageElement(img) {
    if (!img) return Promise.reject(new Error('No image selected'));
    var stored = img.getAttribute('data-stored-name') ||
        (img.closest('.lxmf-image-button') && img.closest('.lxmf-image-button').getAttribute('data-stored-name'));
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
    var raw = atob(file.data_base64 || '');
    var bytes = new Uint8Array(raw.length);
    for (var i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
    return new Blob([bytes], { type: file.mime || 'image/png' });
}

function _copyDownloadedImage(file) {
    if (!navigator.clipboard || typeof ClipboardItem === 'undefined') {
        return Promise.reject(new Error('Image copy is not supported on this device'));
    }
    var blob = _blobFromDownloadedFile(file);
    var mime = /^image\//i.test(file.mime || '') ? file.mime : 'image/png';
    return navigator.clipboard.write([new ClipboardItem({ [mime]: blob })]);
}

function _ensureImageViewer() {
    var existing = document.getElementById('rs-image-viewer');
    if (existing) return existing;
    var viewer = document.createElement('div');
    viewer.id = 'rs-image-viewer';
    viewer.className = 'image-viewer lightbox-zoomable';
    viewer.setAttribute('hidden', '');
    viewer.innerHTML =
        '<div class="image-viewer-backdrop" data-image-viewer-close></div>' +
        '<div class="image-viewer-toolbar">' +
            '<button type="button" class="image-viewer-btn" id="image-viewer-close" title="Close" aria-label="Close">' + _imageActionIcon('close') + '</button>' +
            '<button type="button" class="image-viewer-btn" id="image-viewer-copy" title="Copy image" aria-label="Copy image">' + _imageActionIcon('copy') + '</button>' +
            '<button type="button" class="image-viewer-btn" id="image-viewer-save" title="Save image" aria-label="Save image">' + _imageActionIcon('save') + '</button>' +
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
            showToast((err && err.message) || 'Could not copy image', 'toast-orange', 3000);
        });
    });
    viewer.querySelector('#image-viewer-save').addEventListener('click', function(e) {
        e.stopPropagation();
        var source = viewer._sourceImage;
        _fileFromImageElement(source).then(function(file) {
            return RS.saveDownloadedFile(file, { preferPhotos: true }).then(function() {
                showToast(/^image\//i.test(file.mime || '') ? 'Saved to photos!' : 'Saved', 'toast-green', 2500);
            });
        }).catch(function(err) {
            showToast('Save failed: ' + ((err && err.message) || 'error'), 'toast-red', 4000);
        });
    });
    document.addEventListener('keydown', function(e) {
        if (e.key === 'Escape' && viewer.classList.contains('open')) closeImageViewer();
    });
    _wireImageViewerSwipeDismiss(viewer, stage, viewerImg);
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

function openImageViewer(img) {
    if (!img || !img.src) return;
    var viewer = _ensureImageViewer();
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
}

function closeImageViewer() {
    var viewer = document.getElementById('rs-image-viewer');
    if (!viewer || !viewer.classList.contains('open')) return false;
    viewer.classList.remove('open');
    viewer.classList.remove('is-zoomed');
    document.body.classList.remove('image-viewer-open');
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
