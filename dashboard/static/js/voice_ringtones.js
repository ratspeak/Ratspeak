(function() {
    window.RS = window.RS || {};

    var GROUPS = [4, 4, 3, 2, 1, 1, 1];
    var DOOT_MS = 185;
    var DOOT_SPACING_MS = 292;
    var GROUP_PAUSE_MS = 1780;
    var FINAL_PAUSE_MS = 1040;
    var OUTGOING_ROOT = 392.0;
    var INCOMING_ROOT = 440.0;
    var activeMode = null;
    var activeToken = null;
    var timedOutToken = null;
    var sequenceId = 0;
    var timers = [];
    var activeNodes = [];
    var masterGain = null;
    var handlers = {};

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
        timers.push(setTimeout(fn, ms));
    }

    function _callToken(mode, call) {
        if (!call) return '';
        return mode + ':' + (call.link_id || call.remote_identity || 'unknown');
    }

    function _shouldRingOutgoing(active) {
        if (!active || active.role !== 'outgoing') return false;
        if (active.status === 'established' || active.status === 'busy' || active.status === 'rejected') return false;
        return true;
    }

    function _desired(state) {
        state = state || {};
        if (state.incoming && !state.active) {
            return { mode: 'incoming', call: state.incoming };
        }
        if (_shouldRingOutgoing(state.active)) {
            return { mode: 'outgoing', call: state.active };
        }
        return null;
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
        var duration = (opts.durationMs || DOOT_MS) / 1000;
        var volume = opts.volume || 0.12;
        var drift = opts.drift || 0;

        var filter = ctx.createBiquadFilter();
        filter.type = 'lowpass';
        filter.frequency.setValueAtTime(opts.filterHz || 1680, t);
        filter.Q.setValueAtTime(0.72, t);

        var envelope = ctx.createGain();
        envelope.gain.setValueAtTime(0.0001, t);
        envelope.gain.exponentialRampToValueAtTime(volume, t + 0.014);
        envelope.gain.exponentialRampToValueAtTime(0.0001, t + duration);

        var body = ctx.createOscillator();
        body.type = 'triangle';
        body.frequency.setValueAtTime(freq, t);
        if (drift) body.frequency.linearRampToValueAtTime(freq * drift, t + duration);
        body.detune.setValueAtTime(opts.detune || 0, t);

        var airGain = ctx.createGain();
        airGain.gain.setValueAtTime(0.32, t);
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
        var root = mode === 'incoming' ? INCOMING_ROOT : OUTGOING_ROOT;
        var phrase = [1, 1.05946, 1.12246, 1.05946];
        var taperLift = Math.max(0, groupIndex - 2) * 0.006;
        return root * phrase[noteIndex % phrase.length] * (1 + taperLift);
    }

    function _playDoot(mode, groupIndex, noteIndex) {
        var incoming = mode === 'incoming';
        _playTone(_dootFrequency(mode, groupIndex, noteIndex), {
            durationMs: DOOT_MS,
            volume: incoming ? 0.16 : 0.13,
            filterHz: incoming ? 1840 : 1590,
            overtone: incoming ? 1.498 : 1.333,
            detune: ((groupIndex + noteIndex) % 2 === 0) ? -3 : 3,
            drift: incoming ? 1.004 : 0.997
        });
    }

    function _sequenceLengthMs() {
        var cursor = 0;
        GROUPS.forEach(function(count, idx) {
            cursor += ((count - 1) * DOOT_SPACING_MS) + DOOT_MS;
            cursor += idx === GROUPS.length - 1 ? FINAL_PAUSE_MS : GROUP_PAUSE_MS;
        });
        return cursor;
    }

    function _scheduleSequence(mode, token, id) {
        var cursor = 0;
        GROUPS.forEach(function(count, groupIndex) {
            for (var i = 0; i < count; i++) {
                (function(noteIndex, delay) {
                    _schedule(function() {
                        if (sequenceId !== id || activeToken !== token || activeMode !== mode) return;
                        _playDoot(mode, groupIndex, noteIndex);
                    }, delay);
                })(i, cursor + (i * DOOT_SPACING_MS));
            }
            cursor += ((count - 1) * DOOT_SPACING_MS) + DOOT_MS;
            cursor += groupIndex === GROUPS.length - 1 ? FINAL_PAUSE_MS : GROUP_PAUSE_MS;
        });

        _schedule(function() {
            if (sequenceId !== id || activeToken !== token || activeMode !== mode) return;
            if (mode === 'incoming') {
                _clearTimers();
                _scheduleSequence(mode, token, id);
                return;
            }
            timedOutToken = token;
            activeMode = null;
            activeToken = null;
            _clearTimers();
            playTimeoutCue();
            if (typeof handlers.onOutgoingTimeout === 'function') {
                try { handlers.onOutgoingTimeout(); } catch (err) { window.RS.diag('warn', '[ringtone] timeout handler failed:', err); }
            }
        }, cursor);
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
        _ensureAudio().then(function() {
            if (sequenceId !== id || activeMode !== mode || activeToken !== token) return;
            _scheduleSequence(mode, token, id);
        });
    }

    function stop() {
        sequenceId++;
        activeMode = null;
        activeToken = null;
        _clearTimers();
        _stopNodes();
    }

    function playTimeoutCue() {
        _ensureAudio().then(function() {
            var root = OUTGOING_ROOT * 1.12246;
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
