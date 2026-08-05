// Applies appearance before CSS paints. Keep this file dependency-free: it runs
// in <head>, before the rest of the dashboard runtime is available.
(function() {
    'use strict';

    var MODE_STORAGE_KEY = 'rs-theme';
    var FAMILY_STORAGE_KEY = 'rs-theme-family';
    var DEFAULT_FAMILY = 'ratspeak';
    var DEFAULT_MODE = 'auto';
    var MODES = ['light', 'auto', 'dark'];
    var lastNativeMode = null;
    var FAMILIES = [
        {
            id: 'ratspeak',
            name: 'Ratspeak',
            preview: {
                light: ['#FAF7F3', '#FFFFFF', '#B14F24'],
                dark: ['#18171A', '#1D1B1E', '#D2693B']
            }
        },
        {
            id: 'nord',
            name: 'Nord',
            preview: {
                light: ['#ECEFF4', '#F8FAFC', '#46658A'],
                dark: ['#252A34', '#3B4252', '#88C0D0']
            }
        },
        {
            id: 'everforest',
            name: 'Everforest',
            preview: {
                light: ['#EFEBD4', '#FDF6E3', '#526B2F'],
                dark: ['#232A2E', '#2D353B', '#A7C080']
            }
        },
        {
            id: 'gruvbox',
            name: 'Gruvbox',
            preview: {
                light: ['#FBF1C7', '#F9F5D7', '#076678'],
                dark: ['#1D2021', '#32302F', '#83A598']
            }
        },
        {
            id: 'catppuccin',
            name: 'Catppuccin',
            preview: {
                light: ['#EFF1F5', '#F8F9FC', '#8839EF'],
                dark: ['#11111B', '#313244', '#CBA6F7']
            }
        }
    ];

    function isFamily(value) {
        return FAMILIES.some(function(family) { return family.id === value; });
    }

    function normalizeFamily(value) {
        // Solarized shipped in an early preview. Preserve that preference by
        // moving it to the family that replaced it instead of silently falling
        // back to Ratspeak on the next cold start.
        if (value === 'solarized') return 'everforest';
        return isFamily(value) ? value : DEFAULT_FAMILY;
    }

    function normalizeMode(value) {
        return MODES.indexOf(value) !== -1 ? value : DEFAULT_MODE;
    }

    function readStored(key) {
        try { return localStorage.getItem(key); } catch (_) {}
        return null;
    }

    function removeStored(key) {
        try { localStorage.removeItem(key); } catch (_) {}
    }

    function storedFamily() {
        var value = readStored(FAMILY_STORAGE_KEY);
        var normalized = normalizeFamily(value);
        if (value === 'solarized') {
            try { localStorage.setItem(FAMILY_STORAGE_KEY, normalized); } catch (_) {}
        } else if (value && !isFamily(value)) {
            removeStored(FAMILY_STORAGE_KEY);
        }
        return normalized;
    }

    function storedMode() {
        var value = readStored(MODE_STORAGE_KEY);
        if (value && MODES.indexOf(value) === -1) removeStored(MODE_STORAGE_KEY);
        return normalizeMode(value);
    }

    function resolvedMode(preference) {
        var mode = normalizeMode(preference);
        if (mode !== 'auto') return mode;
        var prefersDark = window.matchMedia &&
            window.matchMedia('(prefers-color-scheme: dark)').matches;
        return prefersDark ? 'dark' : 'light';
    }

    function familyById(id) {
        var normalized = normalizeFamily(id);
        for (var i = 0; i < FAMILIES.length; i += 1) {
            if (FAMILIES[i].id === normalized) return FAMILIES[i];
        }
        return FAMILIES[0];
    }

    function updateThemeColor(family, mode) {
        var meta = document.querySelector('meta[name="theme-color"]');
        if (!meta) return;
        var entry = familyById(family);
        var preview = entry.preview[mode] || entry.preview.light;
        meta.setAttribute('content', preview[0]);
    }

    function syncNativeMode(mode) {
        if (lastNativeMode === mode) return;
        if (window.RatspeakAndroid &&
            typeof window.RatspeakAndroid.setColorMode === 'function') {
            try {
                window.RatspeakAndroid.setColorMode(mode);
                lastNativeMode = mode;
            } catch (_) {}
            return;
        }
        if (window.__RATSPEAK_DESKTOP__ === true && window.RS &&
            typeof window.RS.invoke === 'function') {
            lastNativeMode = mode;
            window.RS.invoke('set_native_theme', { theme: mode }).catch(function() {
                lastNativeMode = null;
            });
        }
    }

    function writePreference(family, mode) {
        try {
            if (family === DEFAULT_FAMILY) localStorage.removeItem(FAMILY_STORAGE_KEY);
            else localStorage.setItem(FAMILY_STORAGE_KEY, family);
            if (mode === DEFAULT_MODE) localStorage.removeItem(MODE_STORAGE_KEY);
            else localStorage.setItem(MODE_STORAGE_KEY, mode);
        } catch (_) {}
    }

    function apply(familyValue, modeValue, committed, announce) {
        var family = normalizeFamily(familyValue);
        var preference = normalizeMode(modeValue);
        var mode = resolvedMode(preference);
        var root = document.documentElement;

        root.setAttribute('data-theme-family', family);
        root.setAttribute('data-theme', mode);
        root.setAttribute('data-theme-preference', preference);

        if (committed) writePreference(family, preference);
        updateThemeColor(family, mode);
        syncNativeMode(mode);

        if (announce !== false && typeof window.CustomEvent === 'function') {
            window.dispatchEvent(new CustomEvent('ratspeak-theme-changed', {
                detail: {
                    family: family,
                    preference: preference,
                    mode: mode,
                    committed: !!committed
                }
            }));
        }
        return { family: family, preference: preference, mode: mode };
    }

    window.RS = window.RS || {};
    window.RS.appearance = {
        DEFAULT_FAMILY: DEFAULT_FAMILY,
        DEFAULT_MODE: DEFAULT_MODE,
        families: FAMILIES,
        modes: MODES.slice(),
        get: function() {
            return {
                family: normalizeFamily(document.documentElement.getAttribute('data-theme-family')),
                preference: normalizeMode(document.documentElement.getAttribute('data-theme-preference')),
                mode: document.documentElement.getAttribute('data-theme') || 'light'
            };
        },
        preview: function(family, mode) { return apply(family, mode, false, true); },
        commit: function(family, mode) { return apply(family, mode, true, true); },
        normalizeFamily: normalizeFamily,
        normalizeMode: normalizeMode
    };

    // Compatibility for callers that only know about the original mode API.
    window.setTheme = function(mode) {
        return window.RS.appearance.commit(window.RS.appearance.get().family, mode);
    };
    window.setThemeFamily = function(family) {
        var current = window.RS.appearance.get();
        return window.RS.appearance.commit(family, current.preference);
    };
    window.getTheme = function() {
        return window.RS.appearance.get().mode;
    };
    window.getThemePreference = function() {
        return window.RS.appearance.get().preference;
    };
    window.getThemeFamily = function() {
        return window.RS.appearance.get().family;
    };

    apply(storedFamily(), storedMode(), false, false);

    if (window.matchMedia) {
        var colorSchemeQuery = window.matchMedia('(prefers-color-scheme: dark)');
        var handleColorSchemeChange = function() {
            var current = window.RS.appearance.get();
            if (current.preference === 'auto') apply(current.family, 'auto', false, true);
        };
        if (typeof colorSchemeQuery.addEventListener === 'function') {
            colorSchemeQuery.addEventListener('change', handleColorSchemeChange);
        } else if (typeof colorSchemeQuery.addListener === 'function') {
            colorSchemeQuery.addListener(handleColorSchemeChange);
        }
    }

    document.addEventListener('DOMContentLoaded', function() {
        lastNativeMode = null;
        syncNativeMode(window.RS.appearance.get().mode);
    });
})();
