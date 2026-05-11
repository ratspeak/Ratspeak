(function() {
    window.RS = window.RS || {};

    var OUTGOING_GROUPS = [2, 2];
    var INCOMING_GROUPS = [2, 2];
    var OUTGOING_DOOT_MS = 145;
    var INCOMING_DOOT_MS = 145;
    var OUTGOING_SPACING_MS = 225;
    var INCOMING_SPACING_MS = 225;
    var OUTGOING_GROUP_PAUSE_MS = 720;
    var INCOMING_GROUP_PAUSE_MS = 720;
    var OUTGOING_FINAL_PAUSE_MS = 1536;
    var INCOMING_FINAL_PAUSE_MS = 1536;
    var OUTGOING_ROOT = 622.25;
    var INCOMING_ROOT = 622.25;
    var OUTGOING_VOLUME = 0.17;
    var INCOMING_VOLUME = 0.22;
    var NO_ANSWER_ROOT = 440.0;
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
            bridge.playCallRingtone(mode);
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
        return active.status === 'ringing';
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
            groups: incoming ? INCOMING_GROUPS : OUTGOING_GROUPS,
            dootMs: incoming ? INCOMING_DOOT_MS : OUTGOING_DOOT_MS,
            spacingMs: incoming ? INCOMING_SPACING_MS : OUTGOING_SPACING_MS,
            groupPauseMs: incoming ? INCOMING_GROUP_PAUSE_MS : OUTGOING_GROUP_PAUSE_MS,
            finalPauseMs: incoming ? INCOMING_FINAL_PAUSE_MS : OUTGOING_FINAL_PAUSE_MS,
            root: incoming ? INCOMING_ROOT : OUTGOING_ROOT,
            phrase: [1, 1.25992],
            volume: incoming ? INCOMING_VOLUME : OUTGOING_VOLUME,
            filterHz: 2350,
            overtone: 1.498,
            drift: 1.006,
            attackMs: 10,
            airGain: 0.30
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

    function _playTone(freq, opts) {
        opts = opts || {};
        var ctx = _ringCtx();
        if (!ctx) return;
        var t = ctx.currentTime + ((opts.delayMs || 0) / 1000);
        var duration = (opts.durationMs || OUTGOING_DOOT_MS) / 1000;
        var volume = opts.volume || 0.12;
        var drift = opts.drift || 0;

        var filter = ctx.createBiquadFilter();
        filter.type = 'lowpass';
        filter.frequency.setValueAtTime(opts.filterHz || 1680, t);
        filter.Q.setValueAtTime(0.72, t);

        var envelope = ctx.createGain();
        envelope.gain.setValueAtTime(0.0001, t);
        envelope.gain.exponentialRampToValueAtTime(volume, t + ((opts.attackMs || 14) / 1000));
        envelope.gain.exponentialRampToValueAtTime(0.0001, t + duration);

        var body = ctx.createOscillator();
        body.type = 'triangle';
        body.frequency.setValueAtTime(freq, t);
        if (drift) body.frequency.linearRampToValueAtTime(freq * drift, t + duration);
        body.detune.setValueAtTime(opts.detune || 0, t);

        var airGain = ctx.createGain();
        airGain.gain.setValueAtTime(opts.airGain || 0.32, t);
        var air = ctx.createOscillator();
        air.type = 'sine';
        air.frequency.setValueAtTime(freq * (opts.overtone || 1.5), t);
        air.detune.setValueAtTime((opts.detune || 0) * -0.5, t);

        body.connect(filter);
        air.connect(airGain);
        airGain.connect(filter);
        filter.connect(envelope);
        envelope.connect(_master(ctx));

        body.start(t);
        air.start(t);
        body.stop(t + duration + 0.045);
        air.stop(t + duration + 0.045);
        if (!opts.detached) {
            _trackNode(body);
            _trackNode(air);
        }

        var cleanupAt = Math.ceil((duration + 0.18) * 1000);
        setTimeout(function() {
            try { filter.disconnect(); } catch (_) {}
            try { envelope.disconnect(); } catch (_) {}
            try { airGain.disconnect(); } catch (_) {}
        }, cleanupAt);
    }

    function _dootFrequency(mode, groupIndex, noteIndex) {
        var cfg = _modeConfig(mode);
        var taperLift = Math.max(0, groupIndex - 2) * 0.004;
        return cfg.root * cfg.phrase[noteIndex % cfg.phrase.length] * (1 + taperLift);
    }

    function _playDoot(mode, groupIndex, noteIndex) {
        var cfg = _modeConfig(mode);
        _playTone(_dootFrequency(mode, groupIndex, noteIndex), {
            durationMs: cfg.dootMs,
            volume: cfg.volume,
            filterHz: cfg.filterHz,
            overtone: cfg.overtone,
            detune: ((groupIndex + noteIndex) % 2 === 0) ? -3 : 3,
            drift: cfg.drift,
            attackMs: cfg.attackMs,
            airGain: cfg.airGain
        });
    }

    function _handleOutgoingTimeout(mode, token, id) {
        if (sequenceId !== id || activeToken !== token || activeMode !== mode) return;
        timedOutToken = token;
        activeMode = null;
        activeToken = null;
        _clearTimers();
        _stopNativeRingtone();
        playTimeoutCue();
        if (typeof handlers.onOutgoingTimeout === 'function') {
            try { handlers.onOutgoingTimeout(); } catch (err) { window.RS.diag('warn', '[ringtone] timeout handler failed:', err); }
        }
    }

    function _handleSequenceComplete(mode, token, id) {
        if (sequenceId !== id || activeToken !== token || activeMode !== mode) return;
        _scheduleSequence(mode, token, id);
    }

    function _sequenceLengthMs(mode) {
        var cfg = _modeConfig(mode || 'outgoing');
        var cursor = 0;
        cfg.groups.forEach(function(count, idx) {
            cursor += ((count - 1) * cfg.spacingMs) + cfg.dootMs;
            cursor += idx === cfg.groups.length - 1 ? cfg.finalPauseMs : cfg.groupPauseMs;
        });
        return cursor;
    }

    function _scheduleSequence(mode, token, id) {
        var cfg = _modeConfig(mode);
        var cursor = 0;
        cfg.groups.forEach(function(count, groupIndex) {
            for (var i = 0; i < count; i++) {
                (function(noteIndex, delay) {
                    _schedule(function() {
                        if (sequenceId !== id || activeToken !== token || activeMode !== mode) return;
                        _playDoot(mode, groupIndex, noteIndex);
                    }, delay);
                })(i, cursor + (i * cfg.spacingMs));
            }
            cursor += ((count - 1) * cfg.spacingMs) + cfg.dootMs;
            cursor += groupIndex === cfg.groups.length - 1 ? cfg.finalPauseMs : cfg.groupPauseMs;
        });

        _schedule(function() { _handleSequenceComplete(mode, token, id); }, cursor);
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
            if (!ok) {
                activeMode = null;
                activeToken = null;
                return;
            }
            _scheduleSequence(mode, token, id);
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
        _ensureAudio().then(function() {
            var root = NO_ANSWER_ROOT;
            _playTone(root * 1.12246, { durationMs: 92, volume: 0.13, filterHz: 1840, drift: 1.04, detached: true });
            _playTone(root * 1.41421, { delayMs: 96, durationMs: 86, volume: 0.12, filterHz: 1920, drift: 1.06, detached: true });
            _playTone(root * 1.25992, { delayMs: 190, durationMs: 104, volume: 0.115, filterHz: 1760, drift: 0.98, detached: true });
            _playTone(root * 1.68179, { delayMs: 310, durationMs: 150, volume: 0.10, filterHz: 2080, drift: 0.96, detached: true });
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
