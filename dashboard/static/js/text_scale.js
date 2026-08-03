(function() {
    'use strict';

    var STORAGE_KEY = 'rs-text-scale-percent';
    var MIN = 100;
    var MAX = 200;
    var STEP = 10;

    function normalize(value) {
        var numeric = Number(value);
        if (!Number.isFinite(numeric)) numeric = MIN;
        numeric = Math.max(MIN, Math.min(MAX, numeric));
        return Math.round((numeric - MIN) / STEP) * STEP + MIN;
    }

    function storedValue() {
        try { return normalize(localStorage.getItem(STORAGE_KEY)); } catch (_) {}
        return MIN;
    }

    function tier(percent) {
        if (percent >= 180) return 'xlarge';
        if (percent >= 140) return 'large';
        return 'normal';
    }

    function apply(value, committed, announce) {
        var percent = normalize(value);
        var root = document.documentElement;
        root.style.fontSize = percent + '%';
        root.dataset.textScale = String(percent);
        root.dataset.textScaleTier = tier(percent);

        if (committed) {
            try {
                if (percent === MIN) localStorage.removeItem(STORAGE_KEY);
                else localStorage.setItem(STORAGE_KEY, String(percent));
            } catch (_) {}
        }

        if (announce !== false && typeof window.CustomEvent === 'function') {
            window.dispatchEvent(new CustomEvent('ratspeak-text-scale-changed', {
                detail: { percent: percent, committed: !!committed }
            }));
        }
        return percent;
    }

    window.RS = window.RS || {};
    window.RS.textScale = {
        MIN: MIN,
        MAX: MAX,
        STEP: STEP,
        get: function() {
            return normalize(document.documentElement.dataset.textScale || storedValue());
        },
        preview: function(value) { return apply(value, false, true); },
        commit: function(value) { return apply(value, true, true); },
        reset: function() { return apply(MIN, true, true); },
        normalize: normalize
    };

    // Apply before CSS and fonts load so the first rendered frame is already
    // at the user's chosen size.
    apply(storedValue(), false, false);
})();
