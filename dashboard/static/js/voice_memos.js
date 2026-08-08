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
    var MAX_PLAYBACK_ITEMS = 6;
    var MAX_PLAYBACK_BYTES = 36 * 1024 * 1024;
    var activeAudio = null;
    var activeKey = '';
    var recordingTarget = '';
    var pointerStartedRecording = false;
    var mobileAudioSessionActive = false;
    var START_FAILURE_MESSAGE = "Ratspeak couldn't start recording. Check microphone access and the selected input device, then try again.";
    var ICON_PLAY = '<path d="M8 5v14l11-7z"/>';
    var ICON_PAUSE = '<path d="M6 5h4v14H6zM14 5h4v14h-4z"/>';

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
        var rendered = downsample(values, BAR_COUNT);
        waveform.innerHTML = rendered.map(function(value, index) {
            var height = 4 + Math.round(Math.max(0, Math.min(255, Number(value) || 0)) / 255 * 22);
            var liveClass = live && index >= Math.max(0, rendered.length - 5) ? ' class="is-live"' : '';
            return '<span' + liveClass + ' style="--voice-bar-height:' + height + 'px"></span>';
        }).join('');
    }
    function setRecorderState(next) {
        recorderState = next;
        var recorder = el('lxmf-voice-recorder');
        var compose = el('lxmf-compose-bar');
        if (recorder) {
            recorder.dataset.state = next;
            recorder.hidden = next === 'idle';
            recorder.setAttribute('aria-busy', next === 'starting' || next === 'stopping' ? 'true' : 'false');
        }
        if (compose) compose.style.display = next === 'idle' && window.lxmfActiveContact ? '' : 'none';

        var reviewing = next === 'review';
        var capturing = next === 'recording' || next === 'paused';
        var busy = next === 'starting' || next === 'stopping';
        var captureFlow = capturing || busy;
        var liveDot = el('voice-memo-live-dot');
        var play = el('voice-memo-play-btn');
        var pauseButton = el('voice-memo-pause-btn');
        var stop = el('voice-memo-stop-btn');
        var send = el('voice-memo-send-btn');
        if (liveDot) liveDot.hidden = !captureFlow;
        if (play) play.hidden = !reviewing;
        if (pauseButton) pauseButton.hidden = !capturing;
        if (stop) {
            stop.hidden = reviewing;
            stop.disabled = busy;
        }
        if (send) send.hidden = !reviewing;
        if (next === 'idle') {
            var timer = el('voice-memo-timer');
            if (timer) timer.textContent = '0:00';
            liveWaveform = [];
            renderRecorderWaveform([], false);
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
    function stopAnyPlayback() {
        if (!activeAudio) return;
        try { activeAudio.pause(); } catch (_) {}
        activeAudio = null;
        var previous = activeKey;
        activeKey = '';
        if (previous) updatePlayerProgress(previous, 0, false);
        syncPreviewPlayButton(false);
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
        recordingTarget = window.lxmfActiveContact;
        stopAnyPlayback();
        return dismissComposerForRecording().then(function() {
            return RS.mediaPermissions.ensure({ audio: true });
        }).then(function(granted) {
            if (!granted) {
                recordingTarget = '';
                showToast('Microphone access is needed to record a voice message.', 'toast-red', 4200);
                return false;
            }
            if (!startMobileAudioSession()) {
                recordingTarget = '';
                showToast('Audio is in use. Finish the current call, then try recording again.', 'toast-orange', 4200);
                return false;
            }
            setRecorderState('starting');
            return RS.invoke('voice_memo_start').then(function() {
                draft = null;
                paused = false;
                liveWaveform = [];
                syncPauseButton();
                renderRecorderWaveform([], true);
                setRecorderState('recording');
                announce('Recording voice message');
                voiceHaptic('light');
                return true;
            }).catch(function() {
                stopMobileAudioSession();
                recordingTarget = '';
                setRecorderState('idle');
                showToast(START_FAILURE_MESSAGE, 'toast-red', 4500);
                return false;
            });
        });
    }
    function togglePause() {
        if (recorderState !== 'recording' && recorderState !== 'paused') return;
        var nextPaused = !paused;
        RS.invoke('voice_memo_pause', { args: { paused: nextPaused } }).then(function() {
            paused = nextPaused;
            syncPauseButton();
            setRecorderState(paused ? 'paused' : 'recording');
            announce(paused ? 'Recording paused' : 'Recording resumed');
            voiceHaptic('light');
        }).catch(function(error) {
            showToast((error && error.message) || 'Could not update the recording.', 'toast-red', 3500);
        });
    }
    function stopRecording() {
        if (recorderState !== 'recording' && recorderState !== 'paused') return;
        setRecorderState('stopping');
        RS.invoke('voice_memo_stop').then(function(result) {
            stopMobileAudioSession();
            draft = result;
            paused = false;
            var timer = el('voice-memo-timer');
            if (timer) timer.textContent = formatDuration(result.duration_ms);
            renderRecorderWaveform(result.waveform || [], false);
            setRecorderState('review');
            announce('Voice message ready to review');
            voiceHaptic('medium');
        }).catch(function(error) {
            stopMobileAudioSession();
            setRecorderState('idle');
            showToast((error && error.message) || 'Could not finish the voice message.', 'toast-red', 4200);
        });
    }
    function discardRecording() {
        stopAnyPlayback();
        var wasCapturing = recorderState === 'recording' || recorderState === 'paused' || recorderState === 'starting' || recorderState === 'stopping';
        var request = wasCapturing ? RS.invoke('voice_memo_cancel').catch(function() {}) : Promise.resolve();
        return request.then(function() {
            stopMobileAudioSession();
            draft = null;
            paused = false;
            recordingTarget = '';
            setRecorderState('idle');
            announce('Voice message discarded');
            voiceHaptic('light');
        });
    }
    function sendDraft() {
        if (recorderState !== 'review' || !draft || typeof window.sendLxmfVoiceMemo !== 'function') return;
        if (!recordingTarget || recordingTarget !== window.lxmfActiveContact) {
            showToast('This voice message belongs to a different conversation and was discarded.', 'toast-orange', 4200);
            discardRecording();
            return;
        }
        stopAnyPlayback();
        var toSend = draft;
        if (window.sendLxmfVoiceMemo(toSend, recordingTarget)) {
            draft = null;
            recordingTarget = '';
            setRecorderState('idle');
            announce('Voice message queued to send');
            voiceHaptic('medium');
        }
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
    function ensurePlayback(key, source) {
        if (playbackByKey[key]) {
            touchPlaybackKey(key);
            return Promise.resolve(playbackByKey[key]);
        }
        return decodeDraftOrStored(source).then(function(result) {
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
        });
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
    function togglePreviewPlayback() {
        if (!draft) return;
        var key = '__voice_memo_draft__';
        var ready = preparePlaybackInteraction();
        if (activeAudio && activeKey === key) {
            ready.then(function(canPlay) {
                if (!canPlay) return;
                if (activeAudio.paused) {
                    activeAudio.play().then(function() { syncPreviewPlayButton(true); }).catch(playbackError);
                } else {
                    activeAudio.pause();
                    syncPreviewPlayButton(false);
                }
            });
            return;
        }
        stopAnyPlayback();
        ready.then(function(canPlay) {
            if (!canPlay) return null;
            return ensurePlayback(key, draft);
        }).then(function(item) {
            if (!item) return null;
            return createPlayback(item).then(function(audio) { return { audio: audio, item: item }; });
        }).then(function(prepared) {
            if (!prepared) return;
            var audio = prepared.audio;
            var item = prepared.item;
            activeAudio = audio;
            activeKey = key;
            audio.addEventListener('timeupdate', function() {
                var timer = el('voice-memo-timer');
                if (timer) timer.textContent = formatDuration(audio.currentTime * 1000);
            });
            audio.addEventListener('ended', function() {
                var timer = el('voice-memo-timer');
                if (timer) timer.textContent = formatDuration(item.duration_ms);
                syncPreviewPlayButton(false);
                activeAudio = null;
                activeKey = '';
            });
            audio.play().then(function() { syncPreviewPlayButton(true); }).catch(playbackError);
        }).catch(function(error) {
            showToast((error && error.message) || 'Could not prepare voice message playback.', 'toast-red', 4000);
        });
    }
    function playbackError(error) {
        window.RS.diag('warn', '[voice memo] playback failed:', error && (error.name || error.message || error));
        showToast('Could not play this voice message.', 'toast-red', 3500);
    }
    function syncPreviewPlayButton(playing) {
        var button = el('voice-memo-play-btn');
        if (!button) return;
        var icon = button.querySelector('.voice-memo-state-icon');
        if (icon) icon.innerHTML = playing ? ICON_PAUSE : ICON_PLAY;
        button.setAttribute('aria-label', playing ? 'Pause voice message' : 'Play voice message');
        button.title = playing ? 'Pause preview' : 'Play preview';
    }

    function renderAttachment(attachment, message) {
        var storedName = attachment.stored_name || '';
        var key = storedName || attachment.voice_memo_key || (message && message.id) || ('memo-' + Math.random());
        var metadata = (storedName && metadataByStoredName[storedName]) || draftByKey[key] || attachment.voice_memo || null;
        var duration = metadata && metadata.duration_ms;
        var waveform = metadata && metadata.waveform;
        var disabled = !storedName && !draftByKey[key];
        return '<div class="voice-memo-player' + (disabled ? ' is-loading' : '') + '" data-voice-key="' + esc(key) + '" data-stored-name="' + esc(storedName) + '">' +
            '<button class="voice-memo-player-play" type="button" aria-label="Play voice message"' + (disabled ? ' disabled' : '') + '>' +
                '<svg class="voice-memo-player-icon" width="17" height="17" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">' + ICON_PLAY + '</svg>' +
            '</button>' +
            '<div class="voice-memo-player-waveform" role="slider" tabindex="0" aria-label="Voice message position" aria-valuemin="0" aria-valuemax="100" aria-valuenow="0">' + barsHtml(waveform || [], 0) + '</div>' +
            '<span class="voice-memo-player-time">' + (duration ? formatDuration(duration) : '--:--') + '</span>' +
        '</div>';
    }
    function registerDraft(key, value) {
        if (!key || !value) return;
        draftByKey[key] = value;
        setTimeout(function() { delete draftByKey[key]; }, 15 * 60 * 1000);
    }
    function sourceForPlayer(player) {
        var key = player.dataset.voiceKey || '';
        var storedName = player.dataset.storedName || '';
        var local = draftByKey[key];
        if (local) return { data_base64: local.data_base64 };
        if (storedName) return { stored_name: storedName };
        return null;
    }
    function setPlayerPlaying(player, playing) {
        var icon = player.querySelector('.voice-memo-player-icon');
        var button = player.querySelector('.voice-memo-player-play');
        if (icon) icon.innerHTML = playing ? ICON_PAUSE : ICON_PLAY;
        if (button) {
            button.setAttribute('aria-label', playing ? 'Pause voice message' : 'Play voice message');
            button.title = playing ? 'Pause voice message' : 'Play voice message';
        }
    }
    function updatePlayerProgress(key, fraction, playing) {
        var player = document.querySelector('.voice-memo-player[data-voice-key="' + cssEscape(key) + '"]');
        if (!player) return;
        var item = playbackByKey[key] || metadataByStoredName[player.dataset.storedName] || draftByKey[key] || {};
        var waveform = player.querySelector('.voice-memo-player-waveform');
        if (waveform) {
            waveform.innerHTML = barsHtml(item.waveform || [], fraction);
            waveform.setAttribute('aria-valuenow', String(Math.round(fraction * 100)));
        }
        setPlayerPlaying(player, !!playing);
    }
    function cssEscape(value) {
        if (window.CSS && typeof window.CSS.escape === 'function') return window.CSS.escape(String(value));
        return String(value).replace(/(["\\])/g, '\\$1');
    }
    function togglePlayer(player) {
        var key = player.dataset.voiceKey || '';
        var source = sourceForPlayer(player);
        if (!source || !key) return;
        var ready = preparePlaybackInteraction();
        if (activeAudio && activeKey === key) {
            ready.then(function(canPlay) {
                if (!canPlay) return;
                if (activeAudio.paused) {
                    activeAudio.play().then(function() {
                        player.classList.remove('is-error');
                        setPlayerPlaying(player, true);
                    }).catch(function(error) {
                        player.classList.add('is-error');
                        playbackError(error);
                    });
                } else {
                    activeAudio.pause();
                    setPlayerPlaying(player, false);
                }
            });
            return;
        }
        stopAnyPlayback();
        player.classList.add('is-loading');
        ready.then(function(canPlay) {
            if (!canPlay) return null;
            return ensurePlayback(key, source);
        }).then(function(item) {
            if (!item) {
                player.classList.remove('is-loading');
                return null;
            }
            player.classList.remove('is-loading');
            var time = player.querySelector('.voice-memo-player-time');
            if (time) time.textContent = formatDuration(item.duration_ms);
            return createPlayback(item).then(function(audio) {
                return { audio: audio, item: item };
            });
        }).then(function(prepared) {
            if (!prepared) return;
            var audio = prepared.audio;
            var item = prepared.item;
            activeAudio = audio;
            activeKey = key;
            audio.addEventListener('timeupdate', function() {
                var fraction = audio.duration ? audio.currentTime / audio.duration : 0;
                updatePlayerProgress(key, fraction, !audio.paused);
                var currentTime = player.querySelector('.voice-memo-player-time');
                if (currentTime) currentTime.textContent = formatDuration(audio.currentTime * 1000);
            });
            audio.addEventListener('ended', function() {
                updatePlayerProgress(key, 0, false);
                var finalTime = player.querySelector('.voice-memo-player-time');
                if (finalTime) finalTime.textContent = formatDuration(item.duration_ms);
                activeAudio = null;
                activeKey = '';
            });
            audio.play().then(function() {
                player.classList.remove('is-error');
                setPlayerPlaying(player, true);
            }).catch(function(error) {
                player.classList.add('is-error');
                playbackError(error);
            });
        }).catch(function(error) {
            player.classList.remove('is-loading');
            player.classList.add('is-error');
            showToast((error && error.message) || 'Could not decode this voice message.', 'toast-red', 4000);
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
        RS.invoke('voice_memo_inspect_stored', { args: { stored_name: storedName } }).then(function(metadata) {
            metadataByStoredName[storedName] = metadata;
            var current = document.querySelector('.voice-memo-player[data-stored-name="' + cssEscape(storedName) + '"]');
            if (!current) return;
            var waveform = current.querySelector('.voice-memo-player-waveform');
            var time = current.querySelector('.voice-memo-player-time');
            var play = current.querySelector('.voice-memo-player-play');
            if (waveform) waveform.innerHTML = barsHtml(metadata.waveform || [], 0);
            if (time) time.textContent = formatDuration(metadata.duration_ms);
            if (play) play.disabled = false;
            current.classList.remove('is-loading');
        }).catch(function() {
            player.classList.remove('is-loading');
            player.classList.add('is-error');
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
                    }
                });
            }
        });
    }
    function onRecordingEvent(data) {
        if (!data || recorderState === 'idle' || recorderState === 'review') return;
        if (data.state === 'recording') {
            paused = false;
            recorderState = 'recording';
            if (typeof data.level === 'number') {
                liveWaveform.push(data.level);
                if (liveWaveform.length > 240) liveWaveform.shift();
                renderRecorderWaveform(liveWaveform, true);
            }
        } else if (data.state === 'paused') {
            paused = true;
            recorderState = 'paused';
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
            if (document.hidden && recorderState !== 'idle') discardRecording();
        });
        window.addEventListener('pagehide', function() {
            if (recorderState !== 'idle') discardRecording();
        });
        renderRecorderWaveform([], false);
        syncComposer();
        RS.invoke('voice_memo_status').then(function(status) {
            available = true;
            if (status && status.state && status.state !== 'idle') {
                // A WebView reload must never leave an unseen microphone live.
                RS.invoke('voice_memo_cancel').catch(function() {});
                stopMobileAudioSession();
            }
            syncComposer();
        }).catch(function() {
            available = false;
            syncComposer();
        });
        RS.listen('voice_memo_recording', onRecordingEvent).catch(function() {});
    }

    function onConversationChanged(hash) {
        stopAnyPlayback();
        if (!recordingTarget || recordingTarget === hash || recorderState === 'idle') return;
        showToast('Voice message discarded when you changed conversations.', 'toast-orange', 3600);
        discardRecording();
    }

    function cancelForCall() {
        stopAnyPlayback();
        if (recorderState === 'idle') {
            stopMobileAudioSession();
            return Promise.resolve();
        }
        var wasCapturing = recorderState !== 'review';
        var request = wasCapturing ? RS.invoke('voice_memo_cancel').catch(function() {}) : Promise.resolve();
        return request.then(function() {
            stopMobileAudioSession();
            draft = null;
            paused = false;
            recordingTarget = '';
            setRecorderState('idle');
            announce('Voice message recording stopped for the call');
        });
    }

    function handleAudioInterruption() {
        stopAnyPlayback();
        if (recorderState === 'idle') {
            stopMobileAudioSession();
            return Promise.resolve();
        }
        showToast('Recording stopped because another app needed audio.', 'toast-orange', 4200);
        return discardRecording();
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
    };

    if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', init);
    else init();
})();
