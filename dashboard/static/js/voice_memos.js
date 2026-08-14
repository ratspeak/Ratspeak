/* Ratspeak voice messages: native LXST/Opus capture, review, and playback.
 * The native runtime owns microphone and codec state. This module owns only
 * the composer/player interaction so it can share LXMF's proven send path. */
(function() {
    'use strict';

    var BAR_COUNT = 42;
    var recorderState = 'idle';
    var available = false;
    var paused = false;
    var draft = null;
    var liveWaveform = [];
    var draftByKey = Object.create(null);
    var metadataByStoredName = Object.create(null);
    var playbackByKey = Object.create(null);
    var playbackOrder = [];
    var playbackBytes = 0;
    var playbackInFlightByKey = Object.create(null);
    var metadataOrder = [];
    var draftExpiryTokenByKey = Object.create(null);
    var draftExpirySequence = 0;
    var mediaCacheGeneration = 0;
    var MAX_PLAYBACK_ITEMS = 6;
    var MAX_PLAYBACK_BYTES = 36 * 1024 * 1024;
    var MAX_METADATA_ITEMS = 128;
    var activeAudio = null;
    var activeKey = '';
    var recordingTarget = '';
    var recordingOwner = null;
    var recordingGeneration = 0;
    var recordingSessionId = '';
    var recordingStartPromise = null;
    var recordingStartRetirement = null;
    var recordingDiscardPromise = null;
    var recordingSendAdmissionStarted = false;
    var playbackGeneration = 0;
    var playbackCoordinator = null;
    var previewPlaybackState = 'idle';
    var pointerStartedRecording = false;
    var mobileAudioSessionActive = false;
    var iosPlaybackLeaseId = '';
    var iosPlaybackSessionTransition = Promise.resolve();
    var START_FAILURE_MESSAGE = "Ratspeak couldn't start recording. Check microphone access and the selected input device, then try again.";
    var ICON_PLAY = '<path d="M8 5v14l11-7z"/>';
    var ICON_PAUSE = '<path d="M6 5h4v14H6zM14 5h4v14h-4z"/>';

    function canonicalConversationHash(value) {
        if (window.RS && RS.conversationOwner && typeof RS.conversationOwner.canonicalHash === 'function') {
            return RS.conversationOwner.canonicalHash(value);
        }
        return String(value == null ? '' : value).trim().toLowerCase();
    }
    function conversationSnapshot() {
        if (window.RS && RS.conversationOwner && typeof RS.conversationOwner.snapshot === 'function') {
            return RS.conversationOwner.snapshot();
        }
        return { hash: canonicalConversationHash(window.lxmfActiveContact), epoch: 0, identityGeneration: 0 };
    }
    function conversationOwnerIsCurrent(owner) {
        if (window.RS && RS.conversationOwner && typeof RS.conversationOwner.isCurrent === 'function') {
            return RS.conversationOwner.isCurrent(owner);
        }
        return !!owner && canonicalConversationHash(owner.hash) === canonicalConversationHash(window.lxmfActiveContact);
    }
    function conversationIdentityIsCurrent(owner) {
        if (window.RS && RS.conversationOwner && typeof RS.conversationOwner.isIdentityCurrent === 'function') {
            return RS.conversationOwner.isIdentityCurrent(owner);
        }
        return conversationOwnerIsCurrent(owner);
    }
    function recorderOperationIsCurrent(generation, owner) {
        return generation === recordingGeneration && conversationOwnerIsCurrent(owner);
    }
    function recordingCommandArgs(extra) {
        var args = Object.assign({}, extra || {});
        if (recordingSessionId) args.session_id = recordingSessionId;
        return { args: args };
    }
    function recordingSessionFrom(result) {
        return String(result && (result.session_id || result.recording_session_id) || '');
    }

    function el(id) { return document.getElementById(id); }
    function esc(value) {
        if (typeof escapeHtml === 'function') return escapeHtml(String(value == null ? '' : value));
        return String(value == null ? '' : value)
            .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
            .replace(/"/g, '&quot;').replace(/'/g, '&#39;');
    }
    function formatDuration(ms) {
        var total = Math.max(0, Math.round(Number(ms || 0) / 1000));
        var minutes = Math.floor(total / 60);
        var seconds = total % 60;
        return minutes + ':' + String(seconds).padStart(2, '0');
    }
    function isVoiceMemoFilename(filename) {
        return /\.lxvm$/i.test(String(filename || ''));
    }
    function isVoiceMemoAttachment(attachment) {
        return !!(attachment && isVoiceMemoFilename(attachment.filename || attachment.stored_name));
    }
    function announce(text) {
        var node = el('voice-memo-announcer');
        if (!node) return;
        node.textContent = '';
        setTimeout(function() { node.textContent = text; }, 20);
    }
    function alertVoice(text) {
        var node = el('voice-memo-alert');
        if (!node) return;
        node.textContent = '';
        setTimeout(function() { node.textContent = text; }, 20);
    }
    function voiceHaptic(kind) {
        if (typeof window.haptic === 'function') {
            try { window.haptic(kind); } catch (_) {}
        }
    }
    function downsample(values, count) {
        values = Array.isArray(values) ? values : [];
        count = Math.max(1, count || BAR_COUNT);
        if (!values.length) {
            return Array.from({ length: count }, function(_, index) {
                return 34 + Math.round(20 * Math.sin((index + 1) * 0.72) + 12 * Math.sin(index * 0.21));
            });
        }
        var result = [];
        for (var i = 0; i < count; i++) {
            var start = Math.floor(i * values.length / count);
            var end = Math.max(start + 1, Math.floor((i + 1) * values.length / count));
            var peak = 0;
            for (var j = start; j < end && j < values.length; j++) peak = Math.max(peak, Number(values[j]) || 0);
            result.push(peak);
        }
        return result;
    }
    function barsHtml(values, playedFraction) {
        return downsample(values, BAR_COUNT).map(function(value, index) {
            var normalized = Math.max(0, Math.min(255, Number(value) || 0));
            var height = 4 + Math.round(normalized / 255 * 22);
            var played = typeof playedFraction === 'number' && ((index + 0.5) / BAR_COUNT) <= playedFraction;
            return '<span' + (played ? ' class="is-played"' : '') + ' style="--voice-bar-height:' + height + 'px"></span>';
        }).join('');
    }
    function renderRecorderWaveform(values, live) {
        var waveform = el('voice-memo-waveform');
        if (!waveform) return;
        var capturing = live === true || live === 'paused';
        if (!capturing) {
            waveform.innerHTML = downsample(values, BAR_COUNT).map(function(value) {
                var height = 4 + Math.round(Math.max(0, Math.min(255, Number(value) || 0)) / 255 * 22);
                return '<span style="--voice-bar-height:' + height + 'px"></span>';
            }).join('');
            return;
        }

        // A recording is a right-edge timeline, not a decorative animation:
        // new signal enters on the right, previous samples move left, and only
        // captured samples receive the recording color.
        var recent = (Array.isArray(values) ? values : []).slice(-BAR_COUNT);
        var emptyCount = BAR_COUNT - recent.length;
        var slots = Array.from({ length: emptyCount }, function() { return null; }).concat(recent);
        waveform.innerHTML = slots.map(function(value, index) {
            if (value === null) {
                return '<span class="is-empty" style="--voice-bar-height:4px"></span>';
            }
            var height = 4 + Math.round(Math.max(0, Math.min(255, Number(value) || 0)) / 255 * 22);
            var classes = ['is-recorded'];
            if (live === true && index === slots.length - 1) classes.push('is-live');
            return '<span class="' + classes.join(' ') + '" style="--voice-bar-height:' + height + 'px"></span>';
        }).join('');
    }
    function setRecorderState(next) {
        recorderState = next;
        var recorder = el('lxmf-voice-recorder');
        var compose = el('lxmf-compose-bar');
        if (recorder) {
            recorder.dataset.state = next;
            recorder.hidden = next === 'idle';
            recorder.setAttribute('aria-busy', next === 'requesting_permission' || next === 'starting' || next === 'stopping' || next === 'sending' ? 'true' : 'false');
        }
        if (compose) compose.style.display = next === 'idle' && window.lxmfActiveContact ? '' : 'none';

        var reviewing = next === 'review' || next === 'sending';
        var capturing = next === 'recording' || next === 'paused';
        var busy = next === 'requesting_permission' || next === 'starting' || next === 'stopping' || next === 'sending';
        var liveDot = el('voice-memo-live-dot');
        var play = el('voice-memo-play-btn');
        var discard = el('voice-memo-discard-btn');
        var pauseButton = el('voice-memo-pause-btn');
        var stop = el('voice-memo-stop-btn');
        var send = el('voice-memo-send-btn');
        var status = el('voice-memo-inline-status');
        if (status) {
            if (next === 'requesting_permission') status.textContent = 'Waiting for microphone…';
            else if (next === 'starting') status.textContent = 'Starting recording…';
            else if (next === 'error') status.textContent = 'Couldn\'t start recording';
            else status.textContent = '';
        }
        if (liveDot) liveDot.hidden = !capturing;
        if (play) {
            play.hidden = !reviewing;
            play.disabled = next === 'sending';
        }
        if (discard) discard.disabled = next === 'sending';
        if (pauseButton) pauseButton.hidden = !capturing || (next === 'recording' && liveWaveform.length === 0);
        if (stop) {
            stop.hidden = reviewing || next === 'requesting_permission';
            stop.disabled = busy;
        }
        if (send) {
            send.hidden = !reviewing;
            send.disabled = next === 'sending';
        }
        if (next === 'idle') {
            var timer = el('voice-memo-timer');
            if (timer) timer.textContent = '0:00';
            liveWaveform = [];
            renderRecorderWaveform([], false);
        } else if (next === 'recording') {
            renderRecorderWaveform(liveWaveform, true);
        } else if (next === 'paused') {
            renderRecorderWaveform(liveWaveform, 'paused');
        }
        syncComposer();
    }
    function syncPauseButton() {
        var button = el('voice-memo-pause-btn');
        if (!button) return;
        var icon = button.querySelector('.voice-memo-state-icon');
        if (icon) icon.innerHTML = paused ? ICON_PLAY : ICON_PAUSE;
        button.setAttribute('aria-label', paused ? 'Resume voice message recording' : 'Pause voice message recording');
        button.title = paused ? 'Resume recording' : 'Pause recording';
    }
    function syncComposer() {
        var recordButton = el('voice-memo-record-btn');
        var sendButton = el('send-msg-btn');
        var input = el('lxmf-input');
        if (!recordButton || !sendButton) return;
        var hasText = !!(input && input.value.trim());
        var hasAttachment = typeof lxmfPendingFile !== 'undefined' && !!lxmfPendingFile;
        var canRecord = available && recorderState === 'idle' && !hasText && !hasAttachment;
        recordButton.hidden = !canRecord;
        sendButton.hidden = canRecord;
    }
    function voiceCallOwnsAudio() {
        return typeof lxstVoiceState !== 'undefined' && !!(lxstVoiceState.active || lxstVoiceState.incoming);
    }
    function preparePlaybackInteraction() {
        if (voiceCallOwnsAudio()) {
            showToast('Finish the current call before playing a voice message.', 'toast-orange', 4200);
            return Promise.resolve(false);
        }
        if (!window.RS || !RS.audioPlayback || typeof RS.audioPlayback.ensure !== 'function') {
            return Promise.resolve(true);
        }
        // Keep this call in the original tap/click task. WebKit can leave its
        // audio context interrupted after iOS releases the microphone session;
        // resuming here restores playback before native memo decoding completes.
        return RS.audioPlayback.ensure({ installUnlock: true }).then(function() {
            return true;
        }).catch(function(error) {
            window.RS.diag('warn', '[voice memo] playback unlock failed:', error);
            return true;
        });
    }
    function queueIosPlaybackSession(action) {
        iosPlaybackSessionTransition = iosPlaybackSessionTransition.catch(function() {}).then(action);
        return iosPlaybackSessionTransition;
    }
    function startIosPlaybackSession() {
        if (!(typeof isIOS === 'function' && isIOS())) return Promise.resolve(true);
        return queueIosPlaybackSession(function() {
            if (iosPlaybackLeaseId) return true;
            return RS.invoke('voice_memo_playback_session_start').then(function(result) {
                iosPlaybackLeaseId = String(result && (result.lease_id || result.session_id) || '');
                return true;
            });
        });
    }
    function stopIosPlaybackSession() {
        if (!(typeof isIOS === 'function' && isIOS())) return Promise.resolve(true);
        return queueIosPlaybackSession(function() {
            if (!iosPlaybackLeaseId) return true;
            var leaseId = iosPlaybackLeaseId;
            iosPlaybackLeaseId = '';
            return RS.invoke('voice_memo_playback_session_stop', { args: { lease_id: leaseId } }).catch(function(error) {
                if (!iosPlaybackLeaseId) iosPlaybackLeaseId = leaseId;
                window.RS.diag('warn', '[voice memo] iOS playback session release failed:', error);
                return false;
            });
        });
    }
    function playWithAudioSession(audio) {
        return startIosPlaybackSession().then(function() {
            return audio.play();
        }).catch(function(error) {
            return stopIosPlaybackSession().then(function() { throw error; });
        });
    }
    function stopAnyPlayback() {
        playbackGeneration += 1;
        if (playbackCoordinator && playbackCoordinator.watchdog) clearTimeout(playbackCoordinator.watchdog);
        if (activeAudio) {
            try { activeAudio.pause(); } catch (_) {}
        }
        activeAudio = null;
        var previous = activeKey;
        activeKey = '';
        playbackCoordinator = null;
        if (previous) updatePlayerProgress(previous, 0, false, 'idle');
        syncPreviewPlayButton(false, 'idle');
        return stopIosPlaybackSession();
    }
    function startMobileAudioSession() {
        if (!window.RatspeakAndroid || typeof window.RatspeakAndroid.startVoiceMemoAudioSession !== 'function') {
            return true;
        }
        try {
            mobileAudioSessionActive = !!window.RatspeakAndroid.startVoiceMemoAudioSession();
            return mobileAudioSessionActive;
        } catch (error) {
            window.RS.diag('warn', '[voice memo] Android audio focus failed:', error);
            mobileAudioSessionActive = false;
            return false;
        }
    }
    function stopMobileAudioSession() {
        if (!mobileAudioSessionActive && (!window.RatspeakAndroid ||
            typeof window.RatspeakAndroid.stopVoiceMemoAudioSession !== 'function')) return;
        mobileAudioSessionActive = false;
        try {
            if (window.RatspeakAndroid && typeof window.RatspeakAndroid.stopVoiceMemoAudioSession === 'function') {
                window.RatspeakAndroid.stopVoiceMemoAudioSession();
            }
        } catch (error) {
            window.RS.diag('warn', '[voice memo] Android audio focus release failed:', error);
        }
    }
    function dismissComposerForRecording() {
        var input = el('lxmf-input');
        if (window.RS && RS.composer && typeof RS.composer.dismissForReplacement === 'function') {
            return RS.composer.dismissForReplacement(input);
        }
        if (input && document.activeElement === input) input.blur();
        return Promise.resolve();
    }
    function startRecording() {
        if (recorderState !== 'idle' || !window.lxmfActiveContact) return Promise.resolve(false);
        if (voiceCallOwnsAudio()) {
            showToast('Finish the current call before recording a voice message.', 'toast-orange', 4200);
            return Promise.resolve(false);
        }
        var generation = ++recordingGeneration;
        var owner = conversationSnapshot();
        recordingOwner = owner;
        recordingTarget = canonicalConversationHash(owner.hash);
        recordingSessionId = '';
        setRecorderState('requesting_permission');
        return stopAnyPlayback().then(function() {
            if (!recorderOperationIsCurrent(generation, owner)) return false;
            return dismissComposerForRecording();
        }).then(function() {
            if (!recorderOperationIsCurrent(generation, owner)) return false;
            return RS.mediaPermissions.ensure({ audio: true });
        }).then(function(granted) {
            if (!recorderOperationIsCurrent(generation, owner)) return false;
            if (!granted) {
                recordingTarget = '';
                recordingOwner = null;
                showToast('Microphone access is needed to record a voice message.', 'toast-red', 4200);
                setRecorderState('idle');
                return false;
            }
            if (!startMobileAudioSession()) {
                recordingTarget = '';
                recordingOwner = null;
                showToast('Audio is in use. Finish the current call, then try recording again.', 'toast-orange', 4200);
                setRecorderState('idle');
                return false;
            }
            setRecorderState('starting');
            var startPromise = RS.invoke('voice_memo_start');
            recordingStartPromise = startPromise;
            return startPromise.then(function(result) {
                var sessionId = recordingSessionFrom(result);
                if (!sessionId) throw new Error('Recording session was not established');
                if (!recorderOperationIsCurrent(generation, owner)) {
                    if (recordingStartRetirement === startPromise) return false;
                    return RS.invoke('voice_memo_cancel', { args: { session_id: sessionId } }).catch(function() {}).then(function() { return false; });
                }
                recordingSessionId = sessionId;
                draft = null;
                paused = false;
                liveWaveform = [];
                syncPauseButton();
                renderRecorderWaveform([], true);
                setRecorderState('recording');
                announce('Recording voice message');
                voiceHaptic('light');
                return true;
            }).catch(function(error) {
                if (generation !== recordingGeneration) return false;
                stopMobileAudioSession();
                recordingTarget = '';
                recordingOwner = null;
                recordingSessionId = '';
                setRecorderState('idle');
                var audioBusy = error && error.code === 'conflict';
                showToast(audioBusy && error.message ? error.message : START_FAILURE_MESSAGE,
                    audioBusy ? 'toast-orange' : 'toast-red', 4500);
                return false;
            }).finally(function() {
                if (recordingStartPromise === startPromise) recordingStartPromise = null;
            });
        });
    }
    function togglePause() {
        if (recorderState !== 'recording' && recorderState !== 'paused') return;
        var nextPaused = !paused;
        var generation = recordingGeneration;
        var sessionId = recordingSessionId;
        RS.invoke('voice_memo_pause', recordingCommandArgs({ paused: nextPaused })).then(function() {
            if (generation !== recordingGeneration || sessionId !== recordingSessionId) return;
            paused = nextPaused;
            syncPauseButton();
            setRecorderState(paused ? 'paused' : 'recording');
            announce(paused ? 'Recording paused' : 'Recording resumed');
            voiceHaptic('light');
        }).catch(function(error) {
            if (generation === recordingGeneration && sessionId === recordingSessionId) {
                showToast((error && error.message) || 'Could not update the recording.', 'toast-red', 3500);
            }
        });
    }
    function stopRecording() {
        if (recorderState !== 'recording' && recorderState !== 'paused') return;
        setRecorderState('stopping');
        var generation = recordingGeneration;
        var sessionId = recordingSessionId;
        RS.invoke('voice_memo_stop', recordingCommandArgs()).then(function(result) {
            if (generation !== recordingGeneration || sessionId !== recordingSessionId) return;
            stopMobileAudioSession();
            recordingSessionId = '';
            draft = result;
            paused = false;
            var timer = el('voice-memo-timer');
            if (timer) timer.textContent = formatDuration(result.duration_ms);
            renderRecorderWaveform(result.waveform || [], false);
            setRecorderState('review');
            announce('Voice message ready to review');
            voiceHaptic('medium');
        }).catch(function(error) {
            if (generation !== recordingGeneration) return;
            stopMobileAudioSession();
            recordingSessionId = '';
            recordingTarget = '';
            recordingOwner = null;
            setRecorderState('idle');
            showToast((error && error.message) || 'Could not finish the voice message.', 'toast-red', 4200);
        });
    }
    function discardRecording() {
        if (recordingDiscardPromise) return recordingDiscardPromise;
        var generation = ++recordingGeneration;
        var sessionId = recordingSessionId;
        var pendingStart = recordingStartPromise;
        recordingSessionId = '';
        recordingSendAdmissionStarted = false;
        var playbackStopped = stopAnyPlayback();
        var wasCapturing = recorderState === 'recording' || recorderState === 'paused' || recorderState === 'starting' || recorderState === 'stopping';
        var request = Promise.resolve();
        if (wasCapturing && sessionId) {
            request = RS.invoke('voice_memo_cancel', { args: { session_id: sessionId } }).catch(function() {});
        } else if (wasCapturing && pendingStart) {
            recordingStartRetirement = pendingStart;
            setRecorderState('stopping');
            request = pendingStart.then(function(result) {
                var retiringSessionId = recordingSessionFrom(result);
                if (!retiringSessionId) return false;
                return RS.invoke('voice_memo_cancel', { args: { session_id: retiringSessionId } }).catch(function() {});
            }).catch(function() {}).finally(function() {
                if (recordingStartRetirement === pendingStart) recordingStartRetirement = null;
            });
        }
        var discardPromise = Promise.all([playbackStopped, request]).then(function() {
            if (generation !== recordingGeneration) return;
            stopMobileAudioSession();
            draft = null;
            paused = false;
            recordingTarget = '';
            recordingOwner = null;
            setRecorderState('idle');
            announce('Voice message discarded');
            voiceHaptic('light');
        }).finally(function() {
            if (recordingDiscardPromise === discardPromise) recordingDiscardPromise = null;
        });
        recordingDiscardPromise = discardPromise;
        return discardPromise;
    }
    function sendDraft() {
        if (recorderState !== 'review' || !draft || typeof window.sendLxmfVoiceMemo !== 'function') return;
        if (!recordingTarget || !recordingOwner || !conversationOwnerIsCurrent(recordingOwner)) {
            showToast('Voice message discarded after changing conversations.', 'toast-orange', 4200);
            discardRecording();
            return;
        }
        stopAnyPlayback();
        var toSend = draft;
        var generation = recordingGeneration;
        var sendOwner = recordingOwner;
        recordingSendAdmissionStarted = false;
        setRecorderState('sending');
        Promise.resolve(window.sendLxmfVoiceMemo(toSend, recordingTarget, {
            owner: sendOwner,
            isCurrent: function() {
                return generation === recordingGeneration && conversationOwnerIsCurrent(sendOwner);
            },
            onAdmissionStart: function() {
                if (generation === recordingGeneration && conversationIdentityIsCurrent(sendOwner)) {
                    recordingSendAdmissionStarted = true;
                }
            },
        })).then(function() {
            if (generation !== recordingGeneration) return;
            draft = null;
            recordingTarget = '';
            recordingOwner = null;
            recordingSendAdmissionStarted = false;
            setRecorderState('idle');
            announce('Voice message queued to send');
            voiceHaptic('medium');
        }).catch(function() {
            if (generation !== recordingGeneration) return;
            recordingSendAdmissionStarted = false;
            setRecorderState('review');
            showToast('Voice message wasn\'t sent. Try again.', 'toast-red', 4200);
        });
    }
    function retireAdmittedSendUi() {
        if (recorderState !== 'sending' || !recordingSendAdmissionStarted) return false;
        recordingGeneration += 1;
        draft = null;
        paused = false;
        recordingTarget = '';
        recordingOwner = null;
        recordingSessionId = '';
        recordingSendAdmissionStarted = false;
        setRecorderState('idle');
        return true;
    }
    function base64Bytes(base64) {
        var raw = atob(base64 || '');
        var bytes = new Uint8Array(raw.length);
        for (var i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
        return bytes;
    }
    function ensureMediaUrl(item) {
        if (!item.url) {
            item.url = URL.createObjectURL(new Blob([item.wavBytes], { type: item.mime || 'audio/wav' }));
            if (!(typeof isIOS === 'function' && isIOS())) item.wavBytes = null;
        }
        return item.url;
    }
    function decodeDraftOrStored(source) {
        if (source.data_base64) {
            return RS.invoke('voice_memo_decode_data', { args: { data_base64: source.data_base64 } });
        }
        return RS.invoke('voice_memo_decode_stored', { args: { stored_name: source.stored_name } });
    }
    function touchPlaybackKey(key) {
        var index = playbackOrder.indexOf(key);
        if (index !== -1) playbackOrder.splice(index, 1);
        playbackOrder.push(key);
    }
    function trimPlaybackCache() {
        while (playbackOrder.length > MAX_PLAYBACK_ITEMS || playbackBytes > MAX_PLAYBACK_BYTES) {
            var key = playbackOrder[0];
            if (key === activeKey && playbackOrder.length > 1) {
                playbackOrder.push(playbackOrder.shift());
                continue;
            }
            playbackOrder.shift();
            var item = playbackByKey[key];
            if (!item) continue;
            playbackBytes = Math.max(0, playbackBytes - (item.bytes || 0));
            if (item.url) {
                try { URL.revokeObjectURL(item.url); } catch (_) {}
            }
            delete playbackByKey[key];
            if (key === activeKey) stopAnyPlayback();
        }
    }
    function clearInactiveMediaCaches(clearDrafts, clearActive) {
        mediaCacheGeneration += 1;
        Object.keys(playbackByKey).forEach(function(key) {
            if (!clearActive && key === activeKey) return;
            var item = playbackByKey[key];
            if (item && item.url) {
                try { URL.revokeObjectURL(item.url); } catch (_) {}
            }
            delete playbackByKey[key];
        });
        playbackOrder = !clearActive && activeKey && playbackByKey[activeKey] ? [activeKey] : [];
        playbackBytes = !clearActive && activeKey && playbackByKey[activeKey] ? (playbackByKey[activeKey].bytes || 0) : 0;
        metadataByStoredName = Object.create(null);
        metadataOrder = [];
        playbackInFlightByKey = Object.create(null);
        if (clearDrafts) {
            draftByKey = Object.create(null);
            draftExpiryTokenByKey = Object.create(null);
        }
    }
    function ensurePlayback(key, source) {
        if (playbackByKey[key]) {
            touchPlaybackKey(key);
            return Promise.resolve(playbackByKey[key]);
        }
        if (playbackInFlightByKey[key]) return playbackInFlightByKey[key];
        var cacheGeneration = mediaCacheGeneration;
        var decode = decodeDraftOrStored(source).then(function(result) {
            if (cacheGeneration !== mediaCacheGeneration) {
                throw new Error('Voice message decode was superseded');
            }
            var wavBytes = base64Bytes(result.data_base64);
            var item = {
                url: '',
                mime: result.mime || 'audio/wav',
                wavBytes: wavBytes,
                duration_ms: result.duration_ms,
                waveform: result.waveform || [],
                bytes: wavBytes.byteLength,
            };
            playbackByKey[key] = item;
            playbackBytes += item.bytes;
            touchPlaybackKey(key);
            trimPlaybackCache();
            return item;
        }).finally(function() {
            if (playbackInFlightByKey[key] === decode) delete playbackInFlightByKey[key];
        });
        playbackInFlightByKey[key] = decode;
        return decode;
    }
    function createEventedPlaybackHandle() {
        var listeners = Object.create(null);
        return {
            addEventListener: function(name, callback) {
                if (!listeners[name]) listeners[name] = [];
                listeners[name].push(callback);
            },
            _emit: function(name) {
                (listeners[name] || []).slice().forEach(function(callback) {
                    try { callback(); } catch (_) {}
                });
            },
        };
    }
    function decodeWebAudioBuffer(ctx, item) {
        var data = item.wavBytes.buffer.slice(
            item.wavBytes.byteOffset,
            item.wavBytes.byteOffset + item.wavBytes.byteLength
        );
        return new Promise(function(resolve, reject) {
            var settled = false;
            function done(buffer) {
                if (settled) return;
                settled = true;
                resolve(buffer);
            }
            function failed(error) {
                if (settled) return;
                settled = true;
                reject(error || new Error('Web Audio could not decode the voice message'));
            }
            try {
                var result = ctx.decodeAudioData(data, done, failed);
                if (result && typeof result.then === 'function') result.then(done, failed);
            } catch (error) {
                failed(error);
            }
        });
    }
    function createWebAudioPlayback(item) {
        var ctx = window.RS && RS.audioPlayback && typeof RS.audioPlayback.context === 'function'
            ? RS.audioPlayback.context()
            : null;
        if (!ctx) return Promise.reject(new Error('Web Audio is unavailable'));
        return decodeWebAudioBuffer(ctx, item).then(function(buffer) {
            var handle = createEventedPlaybackHandle();
            var source = null;
            var animationFrame = 0;
            var offset = 0;
            var startedAt = 0;
            var intentionallyStopped = false;
            handle.paused = true;
            handle.duration = buffer.duration;

            function currentTime() {
                if (handle.paused) return offset;
                return Math.max(0, Math.min(buffer.duration, ctx.currentTime - startedAt));
            }
            function cancelProgress() {
                if (!animationFrame) return;
                cancelAnimationFrame(animationFrame);
                animationFrame = 0;
            }
            function tick() {
                if (handle.paused) return;
                handle._emit('timeupdate');
                animationFrame = requestAnimationFrame(tick);
            }
            function stopSource() {
                if (!source) return;
                intentionallyStopped = true;
                source.onended = null;
                try { source.stop(); } catch (_) {}
                try { source.disconnect(); } catch (_) {}
                source = null;
            }
            function startSource() {
                stopSource();
                intentionallyStopped = false;
                if (offset >= buffer.duration) offset = 0;
                source = ctx.createBufferSource();
                source.buffer = buffer;
                source.connect(ctx.destination);
                source.onended = function() {
                    var wasIntentional = intentionallyStopped;
                    source = null;
                    if (wasIntentional || handle.paused) return;
                    cancelProgress();
                    offset = 0;
                    handle.paused = true;
                    handle._emit('timeupdate');
                    handle._emit('ended');
                };
                startedAt = ctx.currentTime - offset;
                handle.paused = false;
                source.start(0, offset);
                cancelProgress();
                tick();
            }
            handle.play = function() {
                var ready = window.RS && RS.audioPlayback && typeof RS.audioPlayback.ensure === 'function'
                    ? RS.audioPlayback.ensure({ installUnlock: true })
                    : Promise.resolve(true);
                return Promise.resolve(ready).then(function(canPlay) {
                    if (canPlay === false) throw new Error('Web Audio is not ready');
                    startSource();
                });
            };
            handle.pause = function() {
                if (handle.paused) return;
                offset = currentTime();
                handle.paused = true;
                cancelProgress();
                stopSource();
                handle._emit('timeupdate');
            };
            Object.defineProperty(handle, 'currentTime', {
                get: currentTime,
                set: function(value) {
                    offset = Math.max(0, Math.min(buffer.duration, Number(value) || 0));
                    if (!handle.paused) startSource();
                    handle._emit('timeupdate');
                },
            });
            return handle;
        });
    }
    function createMediaPlayback(item) {
        var audio = new Audio(ensureMediaUrl(item));
        audio.preload = 'auto';
        return Promise.resolve(audio);
    }
    function createPlayback(item) {
        // WKWebView is materially more reliable when PCM decoded by Ratspeak is
        // handed to the already-unlocked Web Audio context. Keep Android and
        // desktop on their established HTMLMediaElement path.
        var sharedAudioReady = window.RS && RS.audioPlayback &&
            typeof RS.audioPlayback.isReady === 'function' && RS.audioPlayback.isReady();
        if (typeof isIOS === 'function' && isIOS() && sharedAudioReady) {
            return createWebAudioPlayback(item).catch(function(error) {
                window.RS.diag('warn', '[voice memo] Web Audio playback unavailable, using media element:', error);
                return createMediaPlayback(item);
            });
        }
        return createMediaPlayback(item);
    }
    function startPreviewAttempt(coordinator) {
        return createPlayback(coordinator.item).then(function(audio) {
            if (coordinator.generation !== playbackGeneration) return false;
            coordinator.audio = audio;
            coordinator.progressProven = false;
            coordinator.baseline = Number(audio.currentTime || 0);
            activeAudio = audio;
            activeKey = coordinator.key;
            playbackCoordinator = coordinator;
            syncPreviewPlayButton(false, coordinator.recoveryCount ? 'recovering' : 'starting');
            audio.addEventListener('timeupdate', function() {
                if (!playbackAttemptIsCurrent(coordinator, audio)) return;
                var timer = el('voice-memo-timer');
                if (timer) timer.textContent = formatDuration(audio.currentTime * 1000);
                if (!coordinator.progressProven && audio.currentTime > coordinator.baseline + 0.02) {
                    coordinator.progressProven = true;
                    clearPlaybackWatchdog(coordinator);
                    syncPreviewPlayButton(true, 'playing');
                }
            });
            audio.addEventListener('ended', function() {
                if (!playbackAttemptIsCurrent(coordinator, audio)) return;
                clearPlaybackWatchdog(coordinator);
                var timer = el('voice-memo-timer');
                if (timer) timer.textContent = formatDuration(coordinator.item.duration_ms);
                syncPreviewPlayButton(false, 'ended');
                activeAudio = null;
                activeKey = '';
                playbackCoordinator = null;
                stopIosPlaybackSession();
            });
            return playWithAudioSession(audio).then(function() {
                if (!playbackAttemptIsCurrent(coordinator, audio)) return false;
                clearPlaybackWatchdog(coordinator);
                coordinator.watchdog = setTimeout(function() {
                    if (!playbackAttemptIsCurrent(coordinator, audio) || coordinator.progressProven) return;
                    if (coordinator.recoveryCount < 1) {
                        coordinator.recoveryCount += 1;
                        syncPreviewPlayButton(false, 'recovering');
                        releasePlaybackAttempt(coordinator).then(function() {
                            if (playbackAttemptIsCurrent(coordinator, audio)) {
                                startPreviewAttempt(coordinator).catch(function(error) {
                                    playbackError(coordinator, audio, error);
                                });
                            }
                        });
                        return;
                    }
                    playbackError(coordinator, audio, new Error('Voice message playback did not start'));
                }, (window.RS && RS.config && RS.config.VOICE_PLAYBACK_START_TIMEOUT) || 2000);
                return true;
            }).catch(function(error) {
                if (!playbackAttemptIsCurrent(coordinator, audio)) return false;
                playbackError(coordinator, audio, error);
                return false;
            });
        });
    }
    function togglePreviewPlayback() {
        if (!draft) return;
        if (previewPlaybackState === 'starting' || previewPlaybackState === 'recovering') return;
        var key = '__voice_memo_draft__';
        var ready = preparePlaybackInteraction();
        if (activeAudio && activeKey === key) {
            var coordinator = playbackCoordinator;
            var audio = activeAudio;
            ready.then(function(canPlay) {
                if (!canPlay || !playbackAttemptIsCurrent(coordinator, audio)) return;
                if (audio.paused) {
                    coordinator.baseline = Number(audio.currentTime || 0);
                    coordinator.progressProven = false;
                    syncPreviewPlayButton(false, 'starting');
                    playWithAudioSession(audio).then(function() {
                        if (!playbackAttemptIsCurrent(coordinator, audio)) return false;
                        clearPlaybackWatchdog(coordinator);
                        coordinator.watchdog = setTimeout(function() {
                            if (playbackAttemptIsCurrent(coordinator, audio) && !coordinator.progressProven) {
                                playbackError(coordinator, audio, new Error('Voice message playback did not start'));
                            }
                        }, (window.RS && RS.config && RS.config.VOICE_PLAYBACK_START_TIMEOUT) || 2000);
                        return true;
                    }).catch(function(error) { playbackError(coordinator, audio, error); });
                } else {
                    audio.pause();
                    clearPlaybackWatchdog(coordinator);
                    syncPreviewPlayButton(false, 'paused');
                    stopIosPlaybackSession();
                }
            });
            return;
        }
        var stopped = stopAnyPlayback();
        var generation = playbackGeneration;
        ready.then(function(canPlay) {
            if (!canPlay) return null;
            return stopped.then(function() { return ensurePlayback(key, draft); });
        }).then(function(item) {
            if (!item || generation !== playbackGeneration) return null;
            var coordinator = {
                generation: playbackGeneration,
                key: key,
                item: item,
                audio: null,
                watchdog: 0,
                progressProven: false,
                recoveryCount: 0,
                baseline: 0,
            };
            playbackCoordinator = coordinator;
            return startPreviewAttempt(coordinator);
        }).catch(function(error) {
            if (generation !== playbackGeneration) return;
            showToast((error && error.message) || 'Could not prepare voice message playback.', 'toast-red', 4000);
        });
    }
    function playbackError(coordinator, audio, error) {
        if (!playbackAttemptIsCurrent(coordinator, audio)) return;
        window.RS.diag('warn', '[voice memo] playback failed:', error && (error.name || error.message || error));
        stopAnyPlayback();
        syncPreviewPlayButton(false, 'error');
        showToast('Could not play this voice message.', 'toast-red', 3500);
    }
    function syncPreviewPlayButton(playing, state) {
        previewPlaybackState = state || (playing ? 'playing' : 'idle');
        var button = el('voice-memo-play-btn');
        if (!button) return;
        var icon = button.querySelector('.voice-memo-state-icon');
        if (icon) icon.innerHTML = playing ? ICON_PAUSE : ICON_PLAY;
        var busy = previewPlaybackState === 'starting' || previewPlaybackState === 'recovering';
        button.disabled = busy || recorderState === 'sending';
        button.dataset.playbackState = previewPlaybackState;
        button.setAttribute('aria-label', playing ? 'Pause voice message' :
            busy ? (previewPlaybackState === 'recovering' ? 'Restoring voice message playback' : 'Starting voice message playback') :
                'Play voice message');
        button.title = playing ? 'Pause preview' : busy ? 'Preparing preview' : 'Play preview';
    }

    function renderAttachment(attachment, message) {
        var storedName = attachment.stored_name || '';
        var key = storedName || attachment.voice_memo_key || (message && message.id) || ('memo-' + Math.random());
        var metadata = (storedName && metadataByStoredName[storedName]) || draftByKey[key] || attachment.voice_memo || null;
        var duration = metadata && metadata.duration_ms;
        var waveform = metadata && metadata.waveform;
        var disabled = !storedName && !draftByKey[key];
        return '<div class="voice-memo-player' + (disabled ? ' is-loading' : '') + '" data-playback-state="' + (disabled ? 'loading' : 'idle') + '" data-voice-key="' + esc(key) + '" data-stored-name="' + esc(storedName) + '">' +
            '<button class="voice-memo-player-play" type="button" aria-label="Play voice message"' + (disabled ? ' disabled' : '') + '>' +
                '<svg class="voice-memo-player-icon" width="17" height="17" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">' + ICON_PLAY + '</svg>' +
                '<span class="loading-spinner voice-memo-player-spinner" aria-hidden="true"></span>' +
            '</button>' +
            '<div class="voice-memo-player-waveform" role="slider" tabindex="-1" aria-disabled="true" aria-label="Voice message position" aria-valuemin="0" aria-valuemax="100" aria-valuenow="0">' + barsHtml(waveform || [], 0) + '</div>' +
            '<span class="voice-memo-player-time">' + (duration ? formatDuration(duration) : 'Loading') + '</span>' +
            '<span class="voice-memo-player-status" role="status" aria-live="polite">' + (disabled ? 'Loading' : '') + '</span>' +
        '</div>';
    }
    function registerDraft(key, value) {
        if (!key || !value) return;
        draftByKey[key] = value;
        var token = ++draftExpirySequence;
        draftExpiryTokenByKey[key] = token;
        setTimeout(function() {
            if (draftExpiryTokenByKey[key] !== token) return;
            delete draftByKey[key];
            delete draftExpiryTokenByKey[key];
        }, 15 * 60 * 1000);
    }
    function sourceForPlayer(player) {
        var key = player.dataset.voiceKey || '';
        var storedName = player.dataset.storedName || '';
        var local = draftByKey[key];
        if (local) return { data_base64: local.data_base64 };
        if (storedName) return { stored_name: storedName };
        return null;
    }
    function setPlayerState(player, state, statusText) {
        if (!player) return;
        player.dataset.playbackState = state;
        player.classList.toggle('is-loading', state === 'loading');
        player.classList.toggle('is-error', state === 'error');
        var icon = player.querySelector('.voice-memo-player-icon');
        var button = player.querySelector('.voice-memo-player-play');
        var waveform = player.querySelector('.voice-memo-player-waveform');
        var status = player.querySelector('.voice-memo-player-status');
        var playing = state === 'playing';
        if (icon) icon.innerHTML = playing ? ICON_PAUSE : ICON_PLAY;
        if (button) {
            var label = playing ? 'Pause voice message' :
                state === 'ended' ? 'Replay voice message' :
                state === 'error' ? 'Try voice message again' :
                state === 'loading' ? 'Loading voice message' :
                state === 'starting' ? 'Starting playback' :
                state === 'recovering' || state === 'stalled' ? 'Restoring playback' :
                'Play voice message';
            button.setAttribute('aria-label', label);
            button.title = label;
            button.disabled = state === 'loading' || state === 'starting' || state === 'recovering' || state === 'stalled';
        }
        if (waveform) {
            var seekAvailable = (state === 'playing' || state === 'paused') &&
                activeAudio && activeKey === player.dataset.voiceKey && isFinite(activeAudio.duration);
            waveform.tabIndex = seekAvailable ? 0 : -1;
            waveform.setAttribute('aria-disabled', seekAvailable ? 'false' : 'true');
        }
        if (status) status.textContent = statusText || (
            state === 'loading' ? 'Loading' :
            state === 'starting' ? 'Starting playback' :
            state === 'recovering' || state === 'stalled' ? 'Restoring playback' :
            state === 'error' ? 'Couldn\'t play' : ''
        );
    }
    function setPlayerPlaying(player, playing) {
        setPlayerState(player, playing ? 'playing' : 'paused');
    }
    function updatePlayerProgress(key, fraction, playing, explicitState) {
        var player = document.querySelector('.voice-memo-player[data-voice-key="' + cssEscape(key) + '"]');
        if (!player) return;
        var item = playbackByKey[key] || metadataByStoredName[player.dataset.storedName] || draftByKey[key] || {};
        var waveform = player.querySelector('.voice-memo-player-waveform');
        if (waveform) {
            waveform.innerHTML = barsHtml(item.waveform || [], fraction);
            waveform.setAttribute('aria-valuenow', String(Math.round(fraction * 100)));
            var durationMs = Number(item.duration_ms || 0);
            waveform.setAttribute('aria-valuetext', formatDuration(fraction * durationMs) + ' of ' + formatDuration(durationMs));
        }
        setPlayerState(player, explicitState || (playing ? 'playing' : 'paused'));
    }
    function cssEscape(value) {
        if (window.CSS && typeof window.CSS.escape === 'function') return window.CSS.escape(String(value));
        return String(value).replace(/(["\\])/g, '\\$1');
    }
    function playbackAttemptIsCurrent(coordinator, audio) {
        return !!coordinator && playbackCoordinator === coordinator &&
            coordinator.generation === playbackGeneration && coordinator.audio === audio;
    }
    function clearPlaybackWatchdog(coordinator) {
        if (!coordinator || !coordinator.watchdog) return;
        clearTimeout(coordinator.watchdog);
        coordinator.watchdog = 0;
    }
    function releasePlaybackAttempt(coordinator) {
        clearPlaybackWatchdog(coordinator);
        if (coordinator && coordinator.audio) {
            try { coordinator.audio.pause(); } catch (_) {}
        }
        return stopIosPlaybackSession();
    }
    function failPlaybackCoordinator(coordinator, message) {
        if (!coordinator || playbackCoordinator !== coordinator) return;
        releasePlaybackAttempt(coordinator);
        setPlayerState(coordinator.player, 'error', message || 'Couldn\'t play');
        activeAudio = null;
        activeKey = '';
        playbackCoordinator = null;
    }
    function attachPlaybackEvents(coordinator, audio) {
        audio.addEventListener('timeupdate', function() {
            if (!playbackAttemptIsCurrent(coordinator, audio)) return;
            var fraction = audio.duration ? audio.currentTime / audio.duration : 0;
            if (!coordinator.progressProven && audio.currentTime > coordinator.baseline + 0.02) {
                coordinator.progressProven = true;
                clearPlaybackWatchdog(coordinator);
                setPlayerState(coordinator.player, 'playing');
            }
            updatePlayerProgress(coordinator.key, fraction, coordinator.progressProven && !audio.paused,
                coordinator.progressProven ? (audio.paused ? 'paused' : 'playing') : coordinator.player.dataset.playbackState);
            var currentTime = coordinator.player.querySelector('.voice-memo-player-time');
            if (currentTime) currentTime.textContent = formatDuration(audio.currentTime * 1000);
        });
        audio.addEventListener('ended', function() {
            if (!playbackAttemptIsCurrent(coordinator, audio)) return;
            clearPlaybackWatchdog(coordinator);
            updatePlayerProgress(coordinator.key, 1, false, 'ended');
            var finalTime = coordinator.player.querySelector('.voice-memo-player-time');
            if (finalTime) finalTime.textContent = formatDuration(coordinator.item.duration_ms);
            activeAudio = null;
            activeKey = '';
            playbackCoordinator = null;
            stopIosPlaybackSession();
        });
    }
    function startPlaybackAttempt(coordinator) {
        if (!coordinator || coordinator.generation !== playbackGeneration) return Promise.resolve(false);
        return createPlayback(coordinator.item).then(function(audio) {
            if (coordinator.generation !== playbackGeneration) return false;
            coordinator.audio = audio;
            coordinator.progressProven = false;
            coordinator.baseline = Number(audio.currentTime || 0);
            activeAudio = audio;
            activeKey = coordinator.key;
            attachPlaybackEvents(coordinator, audio);
            setPlayerState(coordinator.player, coordinator.recoveryCount ? 'recovering' : 'starting');
            return playWithAudioSession(audio).then(function() {
                if (!playbackAttemptIsCurrent(coordinator, audio)) return false;
                clearPlaybackWatchdog(coordinator);
                coordinator.watchdog = setTimeout(function() {
                    if (!playbackAttemptIsCurrent(coordinator, audio) || coordinator.progressProven) return;
                    if (coordinator.recoveryCount < 1) {
                        coordinator.recoveryCount += 1;
                        setPlayerState(coordinator.player, 'recovering', 'Restoring playback');
                        releasePlaybackAttempt(coordinator).then(function() {
                            if (coordinator.generation === playbackGeneration) {
                                startPlaybackAttempt(coordinator).catch(function() {
                                    failPlaybackCoordinator(coordinator, 'Couldn\'t play');
                                });
                            }
                        });
                        return;
                    }
                    failPlaybackCoordinator(coordinator, 'Couldn\'t play');
                }, (window.RS && RS.config && RS.config.VOICE_PLAYBACK_START_TIMEOUT) || 2000);
                return true;
            }).catch(function(error) {
                if (!playbackAttemptIsCurrent(coordinator, audio)) return false;
                failPlaybackCoordinator(coordinator, 'Couldn\'t play');
                window.RS.diag('warn', '[voice memo] playback failed:', error);
                return false;
            });
        });
    }
    function togglePlayer(player) {
        var key = player.dataset.voiceKey || '';
        var source = sourceForPlayer(player);
        if (!source || !key) return;
        var ready = preparePlaybackInteraction();
        if (activeAudio && activeKey === key && playbackCoordinator) {
            var coordinator = playbackCoordinator;
            var audio = activeAudio;
            ready.then(function(canPlay) {
                if (!canPlay || !playbackAttemptIsCurrent(coordinator, audio)) return;
                if (audio.paused) {
                    coordinator.recoveryCount = 0;
                    coordinator.progressProven = false;
                    coordinator.baseline = Number(audio.currentTime || 0);
                    setPlayerState(player, 'starting');
                    playWithAudioSession(audio).then(function() {
                        if (!playbackAttemptIsCurrent(coordinator, audio)) return false;
                        clearPlaybackWatchdog(coordinator);
                        coordinator.watchdog = setTimeout(function() {
                            if (playbackAttemptIsCurrent(coordinator, audio) && !coordinator.progressProven) {
                                failPlaybackCoordinator(coordinator, 'Couldn\'t play');
                            }
                        }, (window.RS && RS.config && RS.config.VOICE_PLAYBACK_START_TIMEOUT) || 2000);
                        return true;
                    }).catch(function(error) {
                        if (playbackAttemptIsCurrent(coordinator, audio)) {
                            failPlaybackCoordinator(coordinator, 'Couldn\'t play');
                        }
                    });
                } else {
                    audio.pause();
                    clearPlaybackWatchdog(coordinator);
                    setPlayerState(player, 'paused');
                    stopIosPlaybackSession();
                }
            });
            return;
        }
        var stopped = stopAnyPlayback();
        var generation = playbackGeneration;
        setPlayerState(player, 'loading');
        ready.then(function(canPlay) {
            if (!canPlay) return null;
            return stopped.then(function() { return ensurePlayback(key, source); });
        }).then(function(item) {
            if (!item || generation !== playbackGeneration) return null;
            var time = player.querySelector('.voice-memo-player-time');
            if (time) time.textContent = formatDuration(item.duration_ms);
            playbackCoordinator = {
                generation: generation,
                key: key,
                item: item,
                player: player,
                audio: null,
                watchdog: 0,
                progressProven: false,
                recoveryCount: 0,
                baseline: 0,
            };
            return startPlaybackAttempt(playbackCoordinator);
        }).catch(function(error) {
            if (generation !== playbackGeneration) return;
            var unavailable = player.dataset.playbackState === 'loading';
            if (unavailable) {
                setPlayerState(player, 'error', 'Voice message unavailable');
                window.RS.diag('warn', '[voice memo] decode failed:', error);
            } else {
                failPlaybackCoordinator(playbackCoordinator, 'Couldn\'t play');
                window.RS.diag('warn', '[voice memo] playback failed:', error);
            }
        });
    }
    function seekPlayer(player, fraction) {
        var key = player.dataset.voiceKey || '';
        if (!activeAudio || activeKey !== key || !isFinite(activeAudio.duration)) return;
        activeAudio.currentTime = Math.max(0, Math.min(1, fraction)) * activeAudio.duration;
    }
    function hydrateMetadata(player) {
        var storedName = player.dataset.storedName || '';
        if (!storedName || metadataByStoredName[storedName]) return;
        var cacheGeneration = mediaCacheGeneration;
        RS.invoke('voice_memo_inspect_stored', { args: { stored_name: storedName } }).then(function(metadata) {
            if (cacheGeneration !== mediaCacheGeneration) return;
            metadataByStoredName[storedName] = metadata;
            var orderIndex = metadataOrder.indexOf(storedName);
            if (orderIndex !== -1) metadataOrder.splice(orderIndex, 1);
            metadataOrder.push(storedName);
            while (metadataOrder.length > MAX_METADATA_ITEMS) {
                delete metadataByStoredName[metadataOrder.shift()];
            }
            var current = document.querySelector('.voice-memo-player[data-stored-name="' + cssEscape(storedName) + '"]');
            if (!current) return;
            var waveform = current.querySelector('.voice-memo-player-waveform');
            var time = current.querySelector('.voice-memo-player-time');
            var play = current.querySelector('.voice-memo-player-play');
            if (waveform) {
                waveform.innerHTML = barsHtml(metadata.waveform || [], 0);
                waveform.tabIndex = -1;
                waveform.setAttribute('aria-disabled', 'true');
                waveform.setAttribute('aria-valuetext', '0:00 of ' + formatDuration(metadata.duration_ms));
            }
            if (time) time.textContent = formatDuration(metadata.duration_ms);
            if (play) play.disabled = false;
            setPlayerState(current, 'idle');
        }).catch(function() {
            if (cacheGeneration !== mediaCacheGeneration) return;
            setPlayerState(player, 'error', 'Voice message unavailable');
        });
    }
    function hydratePlayers(container) {
        if (!container) return;
        container.querySelectorAll('.voice-memo-player').forEach(function(player) {
            if (player.dataset.voiceBound === '1') return;
            player.dataset.voiceBound = '1';
            hydrateMetadata(player);
            var play = player.querySelector('.voice-memo-player-play');
            var waveform = player.querySelector('.voice-memo-player-waveform');
            if (play) play.addEventListener('click', function() { togglePlayer(player); });
            if (waveform) {
                function seekFromEvent(event) {
                    if (waveform.getAttribute('aria-disabled') === 'true') return;
                    var rect = waveform.getBoundingClientRect();
                    if (!rect.width) return;
                    seekPlayer(player, (event.clientX - rect.left) / rect.width);
                }
                waveform.addEventListener('click', seekFromEvent);
                waveform.addEventListener('keydown', function(event) {
                    if (!activeAudio || activeKey !== player.dataset.voiceKey) return;
                    if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
                        event.preventDefault();
                        activeAudio.currentTime = Math.max(0, Math.min(activeAudio.duration, activeAudio.currentTime + (event.key === 'ArrowRight' ? 5 : -5)));
                    } else if (event.key === 'Home' || event.key === 'End') {
                        event.preventDefault();
                        activeAudio.currentTime = event.key === 'Home' ? 0 : activeAudio.duration;
                    }
                });
            }
        });
    }
    function onRecordingEvent(data) {
        if (!data || recorderState === 'idle' || recorderState === 'review') return;
        var eventSessionId = String(data.session_id || data.recording_session_id || '');
        if (!eventSessionId || !recordingSessionId || eventSessionId !== recordingSessionId) return;
        if (data.state === 'recording') {
            paused = false;
            recorderState = 'recording';
            if (typeof data.level === 'number') {
                liveWaveform.push(data.level);
                if (liveWaveform.length > 240) liveWaveform.shift();
                renderRecorderWaveform(liveWaveform, true);
            }
            setRecorderState('recording');
        } else if (data.state === 'paused') {
            paused = true;
            recorderState = 'paused';
            setRecorderState('paused');
        } else if (data.state === 'limit') {
            stopRecording();
            announce('Maximum voice message length reached');
        } else if (data.state === 'error') {
            showToast(data.message || 'Voice message recording stopped.', 'toast-red', 4200);
            discardRecording();
            return;
        } else if (data.state === 'idle' && recorderState !== 'stopping') {
            stopMobileAudioSession();
            draft = null;
            paused = false;
            recordingTarget = '';
            recordingOwner = null;
            recordingSessionId = '';
            setRecorderState('idle');
            return;
        }
        var timer = el('voice-memo-timer');
        if (timer && typeof data.duration_ms === 'number') timer.textContent = formatDuration(data.duration_ms);
        var recorder = el('lxmf-voice-recorder');
        if (recorder && (data.state === 'recording' || data.state === 'paused')) recorder.dataset.state = data.state;
        syncPauseButton();
    }

    function init() {
        var record = el('voice-memo-record-btn');
        var pauseButton = el('voice-memo-pause-btn');
        var stop = el('voice-memo-stop-btn');
        var discard = el('voice-memo-discard-btn');
        var send = el('voice-memo-send-btn');
        var preview = el('voice-memo-play-btn');
        var input = el('lxmf-input');
        if (record) {
            // Starting on pointer-down makes touch-and-hold feel immediate;
            // keyboard activation still uses click for accessibility.
            record.addEventListener('pointerdown', function(event) {
                if (event.button !== undefined && event.button !== 0) return;
                pointerStartedRecording = true;
                startRecording();
            });
            record.addEventListener('click', function() {
                if (pointerStartedRecording) {
                    pointerStartedRecording = false;
                    return;
                }
                startRecording();
            });
            record.addEventListener('pointercancel', function() {
                pointerStartedRecording = false;
            });
        }
        if (pauseButton) pauseButton.addEventListener('click', togglePause);
        if (stop) stop.addEventListener('click', stopRecording);
        if (discard) discard.addEventListener('click', discardRecording);
        if (send) send.addEventListener('click', sendDraft);
        if (preview) preview.addEventListener('click', togglePreviewPlayback);
        if (input) input.addEventListener('input', syncComposer);
        document.addEventListener('visibilitychange', function() {
            if (document.hidden) stopAnyPlayback();
            if (document.hidden && recorderState !== 'idle') {
                if (retireAdmittedSendUi()) return;
                showToast('Voice message discarded while Ratspeak was in the background.', 'toast-orange', 4200);
                alertVoice('Voice message discarded while Ratspeak was in the background');
                discardRecording();
            }
        });
        window.addEventListener('pagehide', function() {
            stopAnyPlayback();
            if (recorderState !== 'idle' && !retireAdmittedSendUi()) discardRecording();
        });
        renderRecorderWaveform([], false);
        syncComposer();
        RS.invoke('voice_memo_status').then(function(status) {
            available = true;
            if (status && status.state && status.state !== 'idle') {
                // A WebView reload must never leave an unseen microphone live.
                if (status.session_id) {
                    RS.invoke('voice_memo_cancel', { args: { session_id: status.session_id } }).catch(function() {});
                }
                stopMobileAudioSession();
            }
            syncComposer();
        }).catch(function() {
            available = false;
            syncComposer();
        });
        RS.listen('voice_memo_recording', onRecordingEvent).catch(function() {});
    }

    function onConversationChanged(hash, reason) {
        stopAnyPlayback();
        if (reason === 'identity_replaced') clearInactiveMediaCaches(true);
        if (!recordingTarget || recorderState === 'idle') return;
        if (canonicalConversationHash(recordingTarget) === canonicalConversationHash(hash) &&
            recordingOwner && conversationOwnerIsCurrent(recordingOwner)) return;
        if (retireAdmittedSendUi()) return;
        var message = reason === 'left_conversation'
            ? 'Voice message discarded after leaving the conversation.'
            : reason === 'identity_replaced'
                ? 'Voice message discarded after changing identities.'
                : 'Voice message discarded after changing conversations.';
        showToast(message, 'toast-orange', 3600);
        discardRecording();
    }

    function cancelForCall() {
        return stopAnyPlayback().then(function() {
            if (recorderState === 'idle') {
                stopMobileAudioSession();
                return false;
            }
            if (retireAdmittedSendUi()) return false;
            return discardRecording().then(function() { return true; });
        }).then(function(hadRecorder) {
            if (hadRecorder === false) return;
            alertVoice('Voice message discarded for the call');
        });
    }

    function handleAudioInterruption() {
        return stopAnyPlayback().then(function() {
            if (recorderState === 'idle') {
                stopMobileAudioSession();
                return;
            }
            if (retireAdmittedSendUi()) return;
            showToast('Recording stopped because another app needed audio.', 'toast-orange', 4200);
            alertVoice('Recording stopped because another app needed audio');
            return discardRecording();
        });
    }

    window.RS = window.RS || {};
    RS.voiceMemos = {
        isAttachment: isVoiceMemoAttachment,
        renderAttachment: renderAttachment,
        registerDraft: registerDraft,
        hydratePlayers: hydratePlayers,
        syncComposer: syncComposer,
        discard: discardRecording,
        stopPlayback: stopAnyPlayback,
        formatDuration: formatDuration,
        onConversationChanged: onConversationChanged,
        cancelForCall: cancelForCall,
        handleAudioInterruption: handleAudioInterruption,
        releaseInactiveMedia: function(critical) {
            if (critical) stopAnyPlayback();
            clearInactiveMediaCaches(false, !!critical);
        },
    };

    if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', init);
    else init();
})();
