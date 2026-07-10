// Runs as IIFE in <head> before CSS to prevent FOUC.
(function() {
    var STORAGE_KEY = 'rs-theme';
    var stored = null;
    try { stored = localStorage.getItem(STORAGE_KEY); } catch(e) {}

    if (stored === 'dark' || stored === 'light') {
        document.documentElement.setAttribute('data-theme', stored);
    } else {
        var prefersDark = window.matchMedia &&
            window.matchMedia('(prefers-color-scheme: dark)').matches;
        document.documentElement.setAttribute('data-theme', prefersDark ? 'dark' : 'light');
    }

    function updateThemeColor() {
        var meta = document.querySelector('meta[name="theme-color"]');
        if (meta) {
            var isDark = document.documentElement.getAttribute('data-theme') === 'dark';
            meta.setAttribute('content', isDark ? '#18171a' : '#FAF7F3');
        }
    }
    updateThemeColor();

    if (window.matchMedia) {
        window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', function(e) {
            var current;
            try { current = localStorage.getItem(STORAGE_KEY); } catch(ex) {}
            if (!current) {
                document.documentElement.setAttribute('data-theme', e.matches ? 'dark' : 'light');
                updateThemeColor();
            }
        });
    }

    window.setTheme = function(theme) {
        if (theme === 'auto') {
            try { localStorage.removeItem(STORAGE_KEY); } catch(e) {}
            var prefersDark = window.matchMedia &&
                window.matchMedia('(prefers-color-scheme: dark)').matches;
            document.documentElement.setAttribute('data-theme', prefersDark ? 'dark' : 'light');
        } else {
            try { localStorage.setItem(STORAGE_KEY, theme); } catch(e) {}
            document.documentElement.setAttribute('data-theme', theme);
        }
        updateThemeColor();
    };

    window.getTheme = function() {
        return document.documentElement.getAttribute('data-theme') || 'light';
    };

    window.getThemePreference = function() {
        var s;
        try { s = localStorage.getItem(STORAGE_KEY); } catch(e) {}
        return s || 'auto';
    };
})();
