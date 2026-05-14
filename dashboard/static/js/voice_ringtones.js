(function() {
    window.RS = window.RS || {};

    var TWO_PI = Math.PI * 2;
    var RATSPEAK_RINGTONE_LOOP_MS = 3200;
    var RATSPEAK_RINGTONE_E5_HZ = 659.255114;
    var RATSPEAK_RINGTONE_G5_HZ = 783.990872;
    var RATSPEAK_RINGTONE_B5_HZ = 987.766603;
    var RATSPEAK_RINGTONE_SECOND_PARTIAL_PHASE = 0.35 * Math.PI;
    var RATSPEAK_RINGTONE_AIR_PARTIAL_PHASE = 0.10 * Math.PI;
    var RATSPEAK_RINGTONE_INCOMING_NOTES = [
        { startMs: 0, freqHz: RATSPEAK_RINGTONE_E5_HZ, durationMs: 112, gain: 1.00 },
        { startMs: 150, freqHz: RATSPEAK_RINGTONE_G5_HZ, durationMs: 112, gain: 1.00 },
        { startMs: 300, freqHz: RATSPEAK_RINGTONE_B5_HZ, durationMs: 168, gain: 1.00 },
        { startMs: 780, freqHz: RATSPEAK_RINGTONE_B5_HZ, durationMs: 84, gain: 0.88 },
        { startMs: 920, freqHz: RATSPEAK_RINGTONE_G5_HZ, durationMs: 112, gain: 0.92 },
        { startMs: 1070, freqHz: RATSPEAK_RINGTONE_E5_HZ, durationMs: 176, gain: 0.96 }
    ];
    var RATSPEAK_RINGTONE_OUTGOING_NOTES = [
        { startMs: 0, freqHz: RATSPEAK_RINGTONE_G5_HZ, durationMs: 118, gain: 0.82 },
        { startMs: 180, freqHz: RATSPEAK_RINGTONE_E5_HZ, durationMs: 190, gain: 0.88 },
        { startMs: 1560, freqHz: RATSPEAK_RINGTONE_G5_HZ, durationMs: 96, gain: 0.68 },
        { startMs: 1710, freqHz: RATSPEAK_RINGTONE_E5_HZ, durationMs: 160, gain: 0.72 }
    ];
    var RATSPEAK_RINGTONE_INCOMING_PARTIALS = [0.74, 0.18, 0.08];
    var RATSPEAK_RINGTONE_OUTGOING_PARTIALS = [0.80, 0.15, 0.05];
    var RATSPEAK_RINGTONE_INCOMING_GAIN = 0.36;
    var RATSPEAK_RINGTONE_OUTGOING_GAIN = 0.18;
    var RATSPEAK_RINGTONE_INCOMING_GLIDE_CENTS = 7.0;
    var RATSPEAK_RINGTONE_OUTGOING_GLIDE_CENTS = -4.0;
    var RATSPEAK_RINGTONE_INCOMING_ATTACK_MS = 6;
    var RATSPEAK_RINGTONE_OUTGOING_ATTACK_MS = 9;
    var RATSPEAK_RINGTONE_INCOMING_RELEASE_MS = 52;
    var RATSPEAK_RINGTONE_OUTGOING_RELEASE_MS = 64;
    var RATSPEAK_TIMEOUT_CUE_NOTES = [
        { startMs: 0, freqHz: RATSPEAK_RINGTONE_B5_HZ, durationMs: 88, gain: 0.82 },
        { startMs: 112, freqHz: RATSPEAK_RINGTONE_G5_HZ, durationMs: 104, gain: 0.74 },
        { startMs: 238, freqHz: RATSPEAK_RINGTONE_E5_HZ, durationMs: 168, gain: 0.68 }
    ];
    var RATSPEAK_TIMEOUT_CUE_MS = 520;
    var OUTGOING_TIMEOUT_MS = 25000;
    var activeMode = null;
    var activeToken = null;
    var timedOutToken = null;
    var sequenceId = 0;
    var timers = [];
    var activeNodes = [];
    var masterGain = null;
    var nativeRingtoneActive = false;
    var handlers = {};

    function _androidRingtoneBridge() {
        if (!window.RatspeakAndroid) return null;
        if (typeof window.RatspeakAndroid.playCallRingtone !== 'function') return null;
        if (typeof window.RatspeakAndroid.stopCallRingtone !== 'function') return null;
        return window.RatspeakAndroid;
    }

    function _ringCtx() {
        if (window.RS.audioPlayback && typeof window.RS.audioPlayback.context === 'function') {
            return window.RS.audioPlayback.context();
        }
        var ctor = window.AudioContext || window.webkitAudioContext || null;
        if (!ctor) return null;
        if (!window.__ratspeakFallbackAudioContext) {
            try { window.__ratspeakFallbackAudioContext = new ctor(); } catch (_) { return null; }
        }
        return window.__ratspeakFallbackAudioContext;
    }

    function _ensureAudio() {
        if (window.RS.audioPlayback && typeof window.RS.audioPlayback.ensure === 'function') {
            return window.RS.audioPlayback.ensure({ installUnlock: true });
        }
        var ctx = _ringCtx();
        if (!ctx) return Promise.resolve(false);
        if (ctx.state === 'suspended' && typeof ctx.resume === 'function') {
            return ctx.resume().then(function() { return true; }).catch(function() { return false; });
        }
        return Promise.resolve(true);
    }

    function _master(ctx) {
        if (masterGain) return masterGain;
        masterGain = ctx.createGain();
        masterGain.gain.setValueAtTime(0.82, ctx.currentTime);
        masterGain.connect(ctx.destination);
        return masterGain;
    }

    function _trackNode(node) {
        activeNodes.push(node);
        node.onended = function() {
            activeNodes = activeNodes.filter(function(n) { return n !== node; });
        };
    }

    function _clearTimers() {
        timers.forEach(function(timer) { clearTimeout(timer); });
        timers = [];
    }

    function _schedule(fn, ms) {
        var timer = setTimeout(function() {
            timers = timers.filter(function(t) { return t !== timer; });
            fn();
        }, ms);
        timers.push(timer);
    }

    function _callToken(mode, call) {
        if (!call) return '';
        return mode + ':' + (call.link_id || call.remote_identity || 'unknown');
    }

    function _startNativeRingtone(mode) {
        var bridge = _androidRingtoneBridge();
        if (!bridge) return false;
        try {
            var started = bridge.playCallRingtone(mode);
            if (started === false) {
                nativeRingtoneActive = false;
                return false;
            }
            nativeRingtoneActive = true;
            return true;
        } catch (err) {
            nativeRingtoneActive = false;
            window.RS.diag('warn', '[ringtone] native Android ringtone failed:', err);
            return false;
        }
    }

    function _stopNativeRingtone() {
        var bridge = _androidRingtoneBridge();
        if (!bridge || !nativeRingtoneActive) return;
        nativeRingtoneActive = false;
        try { bridge.stopCallRingtone(); } catch (_) {}
    }

    function _shouldRingOutgoing(active) {
        if (!active || active.role !== 'outgoing') return false;
        return active.status !== 'established' && active.status !== 'busy' && active.status !== 'rejected';
    }

    function _desired(state) {
        state = state || {};
        if (state.incoming && !state.active) {
            return { mode: 'incoming', call: state.incoming };
        }
        if (state.active && state.active.role === 'incoming' && state.active.status !== 'established') {
            return { mode: 'incoming', call: state.active };
        }
        if (_shouldRingOutgoing(state.active)) {
            return { mode: 'outgoing', call: state.active };
        }
        return null;
    }

    function _modeConfig(mode) {
        var incoming = mode === 'incoming';
        return {
            notes: incoming ? RATSPEAK_RINGTONE_INCOMING_NOTES : RATSPEAK_RINGTONE_OUTGOING_NOTES,
            loopMs: RATSPEAK_RINGTONE_LOOP_MS,
            gain: incoming ? RATSPEAK_RINGTONE_INCOMING_GAIN : RATSPEAK_RINGTONE_OUTGOING_GAIN,
            glideCents: incoming ? RATSPEAK_RINGTONE_INCOMING_GLIDE_CENTS : RATSPEAK_RINGTONE_OUTGOING_GLIDE_CENTS,
            attackMs: incoming ? RATSPEAK_RINGTONE_INCOMING_ATTACK_MS : RATSPEAK_RINGTONE_OUTGOING_ATTACK_MS,
            releaseMs: incoming ? RATSPEAK_RINGTONE_INCOMING_RELEASE_MS : RATSPEAK_RINGTONE_OUTGOING_RELEASE_MS,
            partials: incoming ? RATSPEAK_RINGTONE_INCOMING_PARTIALS : RATSPEAK_RINGTONE_OUTGOING_PARTIALS
        };
    }

    function _stopNodes() {
        var now = 0;
        var ctx = _ringCtx();
        if (ctx) now = ctx.currentTime;
        activeNodes.forEach(function(node) {
            try { node.stop(now); } catch (_) {}
            try { node.disconnect(); } catch (_) {}
        });
        activeNodes = [];
    }

    function _clamp(value, min, max) {
        return Math.min(max, Math.max(min, value));
    }

    function _raisedCosine(progress) {
        var x = _clamp(progress, 0, 1);
        return 0.5 - (0.5 * Math.cos(Math.PI * x));
    }

    function _mixTone(output, sampleRate, note, opts) {
        var sampleCount = Math.max(1, Math.round((sampleRate * note.durationMs) / 1000));
        var startSample = Math.max(0, Math.round((sampleRate * note.startMs) / 1000));
        var phase = 0;
        for (var i = 0; i < sampleCount; i++) {
            var outputIndex = startSample + i;
            if (outputIndex >= output.length) break;
            var progress = sampleCount > 1 ? i / (sampleCount - 1) : 0;
            var elapsedMs = (i * 1000) / sampleRate;
            var remainingMs = ((sampleCount - i - 1) * 1000) / sampleRate;
            var instantFreq = note.freqHz * Math.pow(2, (opts.glideCents * progress) / 1200);
            phase += (TWO_PI * instantFreq) / sampleRate;
            var envelope = _raisedCosine(elapsedMs / opts.attackMs);
            if (remainingMs < opts.releaseMs) {
                envelope *= _raisedCosine(remainingMs / opts.releaseMs);
            }
            var tone = (opts.partials[0] * Math.sin(phase))
                + (opts.partials[1] * Math.sin((phase * 2.0) + RATSPEAK_RINGTONE_SECOND_PARTIAL_PHASE))
                + (opts.partials[2] * Math.sin((phase * 1.5) + RATSPEAK_RINGTONE_AIR_PARTIAL_PHASE));
            var sample = tone * envelope * opts.gain * note.gain;
            output[outputIndex] = _clamp(output[outputIndex] + sample, -1, 1);
        }
    }

    function _renderBuffer(ctx, notes, durationMs, opts) {
        var sampleRate = ctx.sampleRate || 44100;
        var sampleCount = Math.max(1, Math.round((sampleRate * durationMs) / 1000));
        var buffer = ctx.createBuffer(1, sampleCount, sampleRate);
        var output = buffer.getChannelData(0);
        notes.forEach(function(note) { _mixTone(output, sampleRate, note, opts); });
        return buffer;
    }

    function _renderRingtoneBuffer(ctx, mode) {
        var cfg = _modeConfig(mode);
        return _renderBuffer(ctx, cfg.notes, cfg.loopMs, cfg);
    }

    function _startBufferRingtone(mode) {
        var ctx = _ringCtx();
        if (!ctx) return false;
        try {
            var source = ctx.createBufferSource();
            source.buffer = _renderRingtoneBuffer(ctx, mode);
            source.loop = true;
            source.loopStart = 0;
            source.loopEnd = RATSPEAK_RINGTONE_LOOP_MS / 1000;
            source.connect(_master(ctx));
            _trackNode(source);
            source.start(ctx.currentTime);
            return true;
        } catch (err) {
            window.RS.diag('warn', '[ringtone] Web Audio ringtone failed:', err);
            return false;
        }
    }

    function _handleOutgoingTimeout(mode, token, id) {
        if (sequenceId !== id || activeToken !== token || activeMode !== mode) return;
        timedOutToken = token;
        activeMode = null;
        activeToken = null;
        _clearTimers();
        _stopNativeRingtone();
        _stopNodes();
        playTimeoutCue();
        if (typeof handlers.onOutgoingTimeout === 'function') {
            try { handlers.onOutgoingTimeout(); } catch (err) { window.RS.diag('warn', '[ringtone] timeout handler failed:', err); }
        }
    }

    function _sequenceLengthMs(mode) {
        return _modeConfig(mode || 'outgoing').loopMs;
    }

    function start(mode, call) {
        var token = _callToken(mode, call);
        if (timedOutToken === token) return;
        if (activeMode === mode && activeToken === token) return;
        stop();
        activeMode = mode;
        activeToken = token;
        sequenceId++;
        var id = sequenceId;
        if (mode === 'outgoing') {
            _schedule(function() { _handleOutgoingTimeout(mode, token, id); }, OUTGOING_TIMEOUT_MS);
        }
        if (_startNativeRingtone(mode)) {
            return;
        }
        _ensureAudio().then(function(ok) {
            if (sequenceId !== id || activeMode !== mode || activeToken !== token) return;
            if (!ok || !_startBufferRingtone(mode)) {
                activeMode = null;
                activeToken = null;
            }
        });
    }

    function stop() {
        sequenceId++;
        activeMode = null;
        activeToken = null;
        _clearTimers();
        _stopNativeRingtone();
        _stopNodes();
    }

    function playTimeoutCue() {
        _ensureAudio().then(function(ok) {
            if (!ok) return;
            var ctx = _ringCtx();
            if (!ctx) return;
            var source = ctx.createBufferSource();
            source.buffer = _renderBuffer(ctx, RATSPEAK_TIMEOUT_CUE_NOTES, RATSPEAK_TIMEOUT_CUE_MS, {
                gain: 0.20,
                glideCents: -6.0,
                attackMs: 7,
                releaseMs: 58,
                partials: RATSPEAK_RINGTONE_OUTGOING_PARTIALS
            });
            source.connect(_master(ctx));
            _trackNode(source);
            source.start(ctx.currentTime);
        });
    }

    window.RS.ringtones = {
        ensureAudio: _ensureAudio,
        playTimeoutCue: playTimeoutCue,
        setHandlers: function(nextHandlers) {
            handlers = nextHandlers || {};
        },
        sync: function(state) {
            var desired = _desired(state);
            if (!desired) {
                stop();
                return;
            }
            start(desired.mode, desired.call);
        },
        stop: stop,
        sequenceLengthMs: _sequenceLengthMs
    };
})();
