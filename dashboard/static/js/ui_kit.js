// Helpers for the ui-* components: the app's identicon, icons by name, the
// four time formats, the copyable hash, and avatars with presence. Loads early;
// RS.copyText and the toasts are reached only when a copy runs.

// Blockies-style SVG identicon. Adapted from https://github.com/download13/blockies (MIT).
var blockies = (function() {
    function seedRand(seed) {
        var s = [0, 0, 0, 0];
        for (var i = 0; i < seed.length; i++) {
            s[i % 4] = (s[i % 4] << 5) - s[i % 4] + seed.charCodeAt(i);
        }
        return function() {
            var t = s[0] ^ (s[0] << 11);
            s[0] = s[1]; s[1] = s[2]; s[2] = s[3];
            s[3] = (s[3] ^ (s[3] >> 19) ^ t ^ (t >> 8)) >>> 0;
            return s[3] / ((1 << 31) >>> 0);
        };
    }

    function createColor(rand) {
        var h = Math.floor(rand() * 360);
        var s = ((rand() * 60) + 40);
        var l = ((rand() + rand() + rand() + rand()) * 25);
        return 'hsl(' + h + ',' + s + '%,' + l + '%)';
    }

    function createImageData(rand, gridSize) {
        var w = gridSize, h = gridSize;
        var halfW = Math.ceil(w / 2);
        var data = [];
        for (var y = 0; y < h; y++) {
            var row = [];
            for (var x = 0; x < halfW; x++) {
                // 0 = bg, 1 = primary, 2 = spot
                row.push(Math.floor(rand() * 2.3));
            }
            var fullRow = row.slice();
            for (var x = Math.floor(w / 2) - 1; x >= 0; x--) {
                fullRow.push(row[x]);
            }
            data.push(fullRow);
        }
        return data;
    }

    var fn = function(seed, svgSize) {
        var gridSize = 8;
        var rand = seedRand(seed || '');
        var color = createColor(rand);
        var bgcolor = createColor(rand);
        var spotcolor = createColor(rand);
        var grid = createImageData(rand, gridSize);

        var rects = '';
        for (var y = 0; y < gridSize; y++) {
            for (var x = 0; x < gridSize; x++) {
                var val = grid[y][x];
                var fill = val === 0 ? bgcolor : val === 1 ? color : spotcolor;
                rects += '<rect x="' + x + '" y="' + y + '" width="1" height="1" fill="' + fill + '"/>';
            }
        }

        return '<svg xmlns="http://www.w3.org/2000/svg" width="' + svgSize +
            '" height="' + svgSize + '" viewBox="0 0 ' + gridSize + ' ' + gridSize +
            '" shape-rendering="crispEdges" style="display:block;border-radius:50%;clip-path:circle(50% at 50% 50%);overflow:hidden;">' +
            rects + '</svg>';
    };
    return fn;
})();

// Cache avatars per (hash, size) — blockies PRNG + 64 SVG rects is expensive per call.
var _avatarCache = {};
function identityAvatar(hashValue, size) {
    if (!hashValue) {
        var color = 'var(--text-muted)';
        var radius = size / 2;
        return '<svg width="' + size + '" height="' + size + '" viewBox="0 0 ' + size + ' ' + size +
            '" style="display:block;border-radius:50%;clip-path:circle(50% at 50% 50%);overflow:hidden;">' +
            '<circle cx="' + radius + '" cy="' + radius + '" r="' + radius + '" fill="' + color + '" opacity="0.3"/>' +
            '</svg>';
    }
    var key = hashValue + '|' + size;
    if (_avatarCache[key]) return _avatarCache[key];
    var svg = blockies(hashValue, size);
    _avatarCache[key] = svg;
    return svg;
}

(function() {
    'use strict';
    window.RS = window.RS || {};
    var RS = window.RS;
    var kit = RS.kit = RS.kit || {};

    // Stroke glyphs on a 24 px grid.
    var GLYPHS = {
        search: '<circle cx="11" cy="11" r="7"/><line x1="21" y1="21" x2="16.65" y2="16.65"/>',
        sliders: '<line x1="4" y1="7" x2="13" y2="7"/><line x1="19" y1="7" x2="20" y2="7"/><circle cx="16" cy="7" r="2.5"/><line x1="4" y1="17" x2="5" y2="17"/><line x1="11" y1="17" x2="20" y2="17"/><circle cx="8" cy="17" r="2.5"/>',
        x: '<line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/>',
        plus: '<line x1="12" y1="5" x2="12" y2="19"/><line x1="5" y1="12" x2="19" y2="12"/>',
        check: '<polyline points="20 6 9 17 4 12"/>',
        more: '<circle cx="12" cy="12" r="1"/><circle cx="19" cy="12" r="1"/><circle cx="5" cy="12" r="1"/>',
        'chevron-down': '<polyline points="6 9 12 15 18 9"/>',
        'chevron-right': '<polyline points="9 18 15 12 9 6"/>',
        'chevron-left': '<polyline points="15 18 9 12 15 6"/>',
        updown: '<polyline points="7 15 12 20 17 15"/><polyline points="7 9 12 4 17 9"/>',
        copy: '<rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/>',
        compose: '<path d="M12 20h9"/><path d="M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4z"/>',
        'user-plus': '<path d="M16 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2"/><circle cx="8.5" cy="7" r="4"/><line x1="20" y1="8" x2="20" y2="14"/><line x1="23" y1="11" x2="17" y2="11"/>',
        'user-minus': '<path d="M16 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2"/><circle cx="8.5" cy="7" r="4"/><line x1="23" y1="11" x2="17" y2="11"/>',
        phone: '<path d="M22 16.92v3a2 2 0 0 1-2.18 2 19.8 19.8 0 0 1-8.63-3.07 19.5 19.5 0 0 1-6-6A19.8 19.8 0 0 1 2.12 4.2 2 2 0 0 1 4.11 2h3a2 2 0 0 1 2 1.72c.12.91.33 1.8.63 2.65a2 2 0 0 1-.45 2.11L8.09 9.69a16 16 0 0 0 6.22 6.22l1.21-1.2a2 2 0 0 1 2.11-.45c.85.3 1.74.51 2.65.63A2 2 0 0 1 22 16.92z"/>',
        message: '<path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/>',
        trash: '<polyline points="3 6 5 6 21 6"/><path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6"/><path d="M10 11v6M14 11v6"/><path d="M9 6V4a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2"/>',
        ban: '<circle cx="12" cy="12" r="10"/><line x1="4.93" y1="4.93" x2="19.07" y2="19.07"/>',
        activity: '<polyline points="22 12 18 12 15 21 9 3 6 12 2 12"/>',
        announce: '<path d="M3 11l18-8-8 18-2-8-8-2z"/>',
        globe: '<circle cx="12" cy="12" r="10"/><line x1="2" y1="12" x2="22" y2="12"/><path d="M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z"/>',
        bluetooth: '<polyline points="6.5 6.5 17.5 17.5 12 23 12 1 17.5 6.5 6.5 17.5"/>',
        wifi: '<path d="M5 12.55a11 11 0 0 1 14.08 0"/><path d="M1.42 9a16 16 0 0 1 21.16 0"/><path d="M8.53 16.11a6 6 0 0 1 6.95 0"/><line x1="12" y1="20" x2="12.01" y2="20"/>',
        radio: '<circle cx="12" cy="12" r="2"/><path d="M16.24 7.76a6 6 0 0 1 0 8.49M7.76 16.24a6 6 0 0 1 0-8.49M19.07 4.93a10 10 0 0 1 0 14.14M4.93 19.07a10 10 0 0 1 0-14.14"/>',
        server: '<rect x="2" y="3" width="20" height="7" rx="2"/><rect x="2" y="14" width="20" height="7" rx="2"/><line x1="6" y1="6.5" x2="6.01" y2="6.5"/><line x1="6" y1="17.5" x2="6.01" y2="17.5"/>',
        info: '<circle cx="12" cy="12" r="10"/><line x1="12" y1="16" x2="12" y2="12"/><line x1="12" y1="8" x2="12.01" y2="8"/>',
        alert: '<path d="M10.29 3.86L1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z"/><line x1="12" y1="9" x2="12" y2="13"/><line x1="12" y1="17" x2="12.01" y2="17"/>',
        'check-circle': '<path d="M22 11.08V12a10 10 0 1 1-5.93-9.14"/><polyline points="22 4 12 14.01 9 11.01"/>',
        'x-circle': '<circle cx="12" cy="12" r="10"/><line x1="15" y1="9" x2="9" y2="15"/><line x1="9" y1="9" x2="15" y2="15"/>',
        users: '<path d="M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/><path d="M23 21v-2a4 4 0 0 0-3-3.87"/><path d="M16 3.13a4 4 0 0 1 0 7.75"/>',
        home: '<path d="M3 9l9-7 9 7v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/><polyline points="9 22 9 12 15 12 15 22"/>',
        channels: '<path d="M8.5 16.5a6 6 0 0 1 0-9"/><path d="M15.5 7.5a6 6 0 0 1 0 9"/><path d="M5.5 19.5a10 10 0 0 1 0-15"/><path d="M18.5 4.5a10 10 0 0 1 0 15"/><circle cx="12" cy="12" r="1.7"/>',
        games: '<path d="M6 11h4M8 9v4"/><line x1="15" y1="12" x2="15.01" y2="12"/><line x1="18" y1="10" x2="18.01" y2="10"/><path d="M17.32 5H6.68a4 4 0 0 0-3.978 3.59C2.604 9.416 2 14.456 2 16a3 3 0 0 0 3 3c1 0 1.5-.5 2-1l1.414-1.414A2 2 0 0 1 9.828 16h4.344a2 2 0 0 1 1.414.586L17 18c.5.5 1 1 2 1a3 3 0 0 0 3-3c0-1.545-.604-6.584-.685-7.258A4 4 0 0 0 17.32 5z"/>',
        network: '<rect x="2" y="2" width="6" height="6" rx="1"/><rect x="16" y="2" width="6" height="6" rx="1"/><rect x="9" y="16" width="6" height="6" rx="1"/><path d="M5 8v3a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8"/><line x1="12" y1="13" x2="12" y2="16"/>',
        key: '<path d="M2.6 17.4A2 2 0 0 0 2 18.8V21a1 1 0 0 0 1 1h3a1 1 0 0 0 1-1v-1a1 1 0 0 1 1-1h1a1 1 0 0 0 1-1v-1a1 1 0 0 1 1-1h.2a2 2 0 0 0 1.4-.6l.8-.8A6.5 6.5 0 1 0 9.4 10.6z"/><circle cx="16.5" cy="7.5" r="1.5"/>',
        settings: '<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/>',
        menu: '<line x1="4" y1="6" x2="20" y2="6"/><line x1="4" y1="12" x2="20" y2="12"/><line x1="4" y1="18" x2="20" y2="18"/>',
        refresh: '<polyline points="23 4 23 10 17 10"/><polyline points="1 20 1 14 7 14"/><path d="M3.51 9a9 9 0 0 1 14.85-3.36L23 10M1 14l4.64 4.36A9 9 0 0 0 20.49 15"/>',
        pause: '<rect x="6" y="4" width="4" height="16" rx="1"/><rect x="14" y="4" width="4" height="16" rx="1"/>',
        play: '<polygon points="6 4 20 12 6 20 6 4"/>',
        sun: '<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M6.34 17.66l-1.41 1.41M19.07 4.93l-1.41 1.41"/>',
        moon: '<path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z"/>',
        monitor: '<rect x="2" y="3" width="20" height="14" rx="2"/><line x1="8" y1="21" x2="16" y2="21"/><line x1="12" y1="17" x2="12" y2="21"/>',
        qr: '<rect x="3" y="3" width="7" height="7" rx="1"/><rect x="14" y="3" width="7" height="7" rx="1"/><rect x="3" y="14" width="7" height="7" rx="1"/><path d="M14 14h3v3h-3zM20 14v.01M14 20h.01M17 20h4v-3"/>',
        mic: '<path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z"/><path d="M19 10v2a7 7 0 0 1-14 0v-2"/><line x1="12" y1="19" x2="12" y2="23"/>',
        clip: '<path d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48"/>',
        'arrow-up': '<line x1="12" y1="19" x2="12" y2="5"/><polyline points="5 12 12 5 19 12"/>',
        reply: '<polyline points="9 17 4 12 9 7"/><path d="M20 18v-2a4 4 0 0 0-4-4H4"/>',
        hash: '<line x1="4" y1="9" x2="20" y2="9"/><line x1="4" y1="15" x2="20" y2="15"/><line x1="10" y1="3" x2="8" y2="21"/><line x1="16" y1="3" x2="14" y2="21"/>',
        download: '<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/>',
        inbox: '<polyline points="22 12 16 12 14 15 10 15 8 12 2 12"/><path d="M5.45 5.11L2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z"/>',
        lock: '<rect x="3" y="11" width="18" height="11" rx="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/>',
        edit: '<path d="M12 20h9"/><path d="M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4z"/>',
        image: '<rect x="3" y="3" width="18" height="18" rx="2"/><circle cx="8.5" cy="8.5" r="1.5"/><polyline points="21 15 16 10 5 21"/>',
        flag: '<path d="M4 15s1-1 4-1 5 2 8 2 4-1 4-1V3s-1 1-4 1-5-2-8-2-4 1-4 1z"/><line x1="4" y1="22" x2="4" y2="15"/>',
        clock: '<circle cx="12" cy="12" r="10"/><polyline points="12 6 12 12 16 14"/>',
        eye: '<path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z"/><circle cx="12" cy="12" r="3"/>',
        // The Ratspeak mark: a filled face with drooping whiskers, legible at 14-18 px.
        ratspeak: '<g fill="currentColor" stroke="none"><circle cx="6.2" cy="7" r="4.2"/><circle cx="17.8" cy="7" r="4.2"/><path fill-rule="evenodd" d="M12 22.5c-2.3 0-7-4.6-7-9.2C5 9.6 8.1 7.4 12 7.4s7 2.2 7 5.9c0 4.6-4.7 9.2-7 9.2zM9.4 12a1.15 1.15 0 1 0 0 2.3 1.15 1.15 0 0 0 0-2.3zm5.2 0a1.15 1.15 0 1 0 0 2.3 1.15 1.15 0 0 0 0-2.3z"/></g><path d="M5.3 15.1 1.2 16.3M5.9 17.5 1.9 18.9M18.7 15.1 22.8 16.3M18.1 17.5 22.1 18.9" fill="none" stroke-width="1.8"/>',
        sidebar: '<rect x="3" y="3" width="18" height="18" rx="2"/><line x1="9" y1="3" x2="9" y2="21"/>',
        'more-v': '<circle cx="12" cy="5" r="1"/><circle cx="12" cy="12" r="1"/><circle cx="12" cy="19" r="1"/>',
        'message-plus': '<path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/><path d="M12 7v6"/><path d="M9 10h6"/>',
        tag: '<path d="M12.6 2.6A2 2 0 0 0 11.2 2H4a2 2 0 0 0-2 2v7.2a2 2 0 0 0 .6 1.4l8.7 8.7a2.4 2.4 0 0 0 3.4 0l6.6-6.6a2.4 2.4 0 0 0 0-3.4z"/><circle cx="7.5" cy="7.5" r="1"/>',
        'square-check': '<rect x="3" y="3" width="18" height="18" rx="3"/><polyline points="8 12.5 11 15.5 16 9.5"/>',
        smile: '<circle cx="12" cy="12" r="10"/><path d="M8 14s1.5 2 4 2 4-2 4-2"/><line x1="9" y1="9" x2="9.01" y2="9"/><line x1="15" y1="9" x2="15.01" y2="9"/>',
        'smile-plus': '<path d="M22 11v1a10 10 0 1 1-9-10"/><path d="M8 14s1.5 2 4 2 4-2 4-2"/><line x1="9" y1="9" x2="9.01" y2="9"/><line x1="15" y1="9" x2="15.01" y2="9"/><path d="M16 5h6"/><path d="M19 2v6"/>',
        stop: '<rect x="6" y="6" width="12" height="12" rx="2"/>',
        file: '<path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/>',
        camera: '<path d="M23 19a2 2 0 0 1-2 2H3a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h4l2-3h6l2 3h4a2 2 0 0 1 2 2z"/><circle cx="12" cy="13" r="4"/>',
        video: '<path d="m22 8-6 4 6 4V8Z"/><rect x="2" y="6" width="14" height="12" rx="2"/>',
        opportunistic: '<path d="M12 19V5"/><path d="m5 12 7-7 7 7"/>',
        direct: '<path d="M8 19V5"/><path d="m4 9 4-4 4 4"/><path d="M16 5v14"/><path d="m12 15 4 4 4-4"/>',
        envelope: '<rect x="3" y="5" width="18" height="14" rx="2"/><path d="M3 7l9 6 9-6"/>',
        trophy: '<path d="M8 21h8"/><path d="M12 17v4"/><path d="M7 4h10v5a5 5 0 0 1-10 0z"/><path d="M17 5h2.5a1.5 1.5 0 0 1 1.5 1.5V7a4 4 0 0 1-4 4"/><path d="M7 5H4.5A1.5 1.5 0 0 0 3 6.5V7a4 4 0 0 0 4 4"/>'
    };

    function esc(value) {
        return String(value == null ? '' : value).replace(/&/g, '&amp;').replace(/</g, '&lt;')
            .replace(/>/g, '&gt;').replace(/"/g, '&quot;').replace(/'/g, '&#39;');
    }

    // "Address" reads "address" mid-sentence; "LXMF address" keeps its acronym.
    function lowerFirst(text) {
        return /^[A-Z][a-z]/.test(text) ? text.charAt(0).toLowerCase() + text.slice(1) : text;
    }

    // An unknown name returns '' so an empty slot never pushes a label off centre.
    function icon(name, cls) {
        var glyph = Object.prototype.hasOwnProperty.call(GLYPHS, name) ? GLYPHS[name] : '';
        if (!glyph) return '';
        return '<svg class="ui-ico' + (cls ? ' ' + esc(cls) : '') + '" viewBox="0 0 24 24" aria-hidden="true" focusable="false">' + glyph + '</svg>';
    }

    // Times: Unix seconds as the runtime sends them; milliseconds are accepted too.
    var MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
    var DAYS = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'];
    var use12Hour = (function() {
        try {
            var cycle = new Intl.DateTimeFormat(undefined, { hour: 'numeric' }).resolvedOptions().hourCycle;
            return cycle === 'h12' || cycle === 'h11';
        } catch (_) { return false; }
    })();

    function toMs(ts) {
        var value = Number(ts);
        if (!isFinite(value) || value <= 0) return 0;
        return value < 1e11 ? value * 1000 : value;
    }

    function dayDiff(ms, now) {
        var then = new Date(ms), today = new Date(now);
        var a = new Date(then.getFullYear(), then.getMonth(), then.getDate());
        var b = new Date(today.getFullYear(), today.getMonth(), today.getDate());
        return Math.round((b - a) / 86400000);
    }

    function monthDay(date, now) {
        var text = MONTHS[date.getMonth()] + ' ' + date.getDate();
        return date.getFullYear() === new Date(now).getFullYear() ? text : text + ', ' + date.getFullYear();
    }

    function clock(ts) {
        var ms = toMs(ts);
        if (!ms) return '';
        var date = new Date(ms), minutes = String(date.getMinutes()).padStart(2, '0'), hours = date.getHours();
        if (use12Hour) return ((hours % 12) || 12) + ':' + minutes + ' ' + (hours >= 12 ? 'PM' : 'AM');
        return String(hours).padStart(2, '0') + ':' + minutes;
    }

    var time = {
        // Rows: now, 4m, 3h, Yesterday, Tue, Nov 2, Nov 2, 2025.
        row: function(ts) {
            var ms = toMs(ts);
            if (!ms) return '';
            var now = Date.now(), seconds = Math.max(0, (now - ms) / 1000), days = dayDiff(ms, now);
            if (seconds < 60) return 'now';
            if (seconds < 3600) return Math.floor(seconds / 60) + 'm';
            if (days <= 0) return Math.floor(seconds / 3600) + 'h';
            if (days === 1) return 'Yesterday';
            if (days < 7) return DAYS[new Date(ms).getDay()].slice(0, 3);
            return monthDay(new Date(ms), now);
        },
        // Mid-sentence: just now, 4 minutes ago, today at 9:12, yesterday at 9:12, Tuesday at 9:12, Nov 2 at 9:12.
        sentence: function(ts) {
            var ms = toMs(ts);
            if (!ms) return '';
            var now = Date.now(), seconds = Math.max(0, (now - ms) / 1000), days = dayDiff(ms, now);
            if (seconds < 60) return 'just now';
            if (seconds < 3600) {
                var minutes = Math.floor(seconds / 60);
                return minutes + (minutes === 1 ? ' minute ago' : ' minutes ago');
            }
            if (days <= 0) return 'today at ' + clock(ms);
            if (days === 1) return 'yesterday at ' + clock(ms);
            if (days < 7) return DAYS[new Date(ms).getDay()] + ' at ' + clock(ms);
            return monthDay(new Date(ms), now) + ' at ' + clock(ms);
        },
        lastHeard: function(ts) {
            return toMs(ts) ? 'Last heard ' + time.sentence(ts) : 'Not heard yet';
        },
        // Message bubbles: 9:12 or 09:12, following the system clock.
        clock: clock,
        // Date separators: Today, Yesterday, Tuesday, Nov 2.
        date: function(ts) {
            var ms = toMs(ts);
            if (!ms) return '';
            var now = Date.now(), days = dayDiff(ms, now);
            if (days <= 0) return 'Today';
            if (days === 1) return 'Yesterday';
            if (days < 7) return DAYS[new Date(ms).getDay()];
            return monthDay(new Date(ms), now);
        },
        // The tooltip every displayed time carries.
        full: function(ts) {
            var ms = toMs(ts);
            if (!ms) return '';
            var date = new Date(ms);
            return date.getDate() + ' ' + MONTHS[date.getMonth()] + ' ' + date.getFullYear() + ', ' + clock(ms);
        }
    };

    // One ladder for presence dots, group captions, and headers.
    function presenceFor(lastHeard) {
        var ms = toMs(lastHeard);
        if (!ms) return 'older';
        var age = (Date.now() - ms) / 1000;
        if (age < 2 * 3600) return 'active';
        if (age < 24 * 3600) return 'recent';
        return 'older';
    }

    var PRESENCE = {
        active: ['', 'Active'],
        recent: [' ui-presence--warning', 'Seen recently'],
        older: [' ui-presence--older', 'Older / unknown']
    };
    var AVATAR_SIZES = { 24: 'ui-avatar--24', 32: 'ui-avatar--32', 40: 'ui-avatar--40', 48: 'ui-avatar--48', 64: 'ui-avatar--64', row: 'ui-avatar--row', compact: 'ui-avatar--compact' };
    var AVATAR_FACE = { row: 48, compact: 40 };

    // size: 24, 32, 40, 48, 64, 'row' (40 desktop, 48 touch), or 'compact' (32 desktop, 40 touch).
    // presence: 'active', 'recent', 'older', or true for active; omit it for no dot. heard adds
    // "last heard …" to the label.
    function avatar(seed, size, presence, heard) {
        var key = AVATAR_SIZES[size] ? size : 40;
        var face = identityAvatar(seed || '', AVATAR_FACE[key] || key);
        var tier = presence === true ? PRESENCE.active : PRESENCE[presence];
        var label = tier ? tier[1] + (heard ? ', ' + lowerFirst(time.lastHeard(heard)) : '') : '';
        var dot = tier ? '<span class="ui-presence' + tier[0] + '" role="img" aria-label="' + esc(label) + '"></span>' : '';
        return '<span class="ui-avatar ' + AVATAR_SIZES[key] + '">' + face + dot + '</span>';
    }

    // Hashes. Every truncated hash outside a list row is the copy component, and it copies in full.
    function cleanHash(value) { return String(value == null ? '' : value).replace(/\s+/g, ''); }
    function hashGroups(value) { return cleanHash(value).match(/.{1,8}/g) || []; }

    // Mid and short are fixed text, so only full and fit need their own rules.
    var HASH_FORMS = { fit: ' ui-hash--fit', full: ' ui-hash--full', mid: '', short: '' };

    var hash = {
        // 4f9a2c71…e0b3: tight list rows, where the row's detail offers the copy.
        short: function(value) {
            var h = cleanHash(value);
            return h.length <= 12 ? h : h.slice(0, 8) + '…' + h.slice(-4);
        },
        mid: function(value) {
            var h = cleanHash(value);
            return h.length <= 16 ? h : h.slice(0, 8) + '…' + h.slice(-8);
        },
        // Markup in two unbreakable halves, so a wrap is always 2 groups over 2.
        full: function(value) {
            var groups = hashGroups(value);
            if (groups.length <= 2) return esc(groups.join(' '));
            return '<span class="ui-hash-g">' + esc(groups.slice(0, 2).join(' ')) + '</span> ' +
                '<span class="ui-hash-g">' + esc(groups.slice(2).join(' ')) + '</span>';
        },
        // Plain text of a form, for titles and labels.
        text: function(value, form) {
            if (form === 'short') return hash.short(value);
            if (form === 'mid') return hash.mid(value);
            return hashGroups(value).join(' ');
        },
        // The component. opts: { form: 'fit' (default) | 'full' | 'mid' | 'short', noun: 'Address' }.
        copy: function(value, opts) {
            opts = opts || {};
            var full = cleanHash(value);
            if (!full) return '<span class="ui-hash">—</span>';
            var form = Object.prototype.hasOwnProperty.call(HASH_FORMS, opts.form) ? opts.form : 'fit';
            var noun = opts.noun || 'Address';
            var action = esc('Copy ' + lowerFirst(noun));
            var body = form === 'full' ? hash.full(full) : esc(hash.text(full, form === 'fit' ? 'full' : form));
            return '<span class="ui-hash-copy" data-hash="' + esc(full) + '" data-noun="' + esc(noun) + '" data-hash-form="' + form + '">' +
                '<span class="ui-hash' + HASH_FORMS[form] + '" dir="ltr" title="' + action + '">' + body + '</span>' +
                '<button type="button" class="ui-hash-btn" aria-label="' + action + '" title="' + action + '">' + icon('copy') + '</button></span>';
        }
    };

    function copyHash(wrap) {
        var full = wrap.getAttribute('data-hash') || '';
        var noun = wrap.getAttribute('data-noun') || 'Address';
        var pending = typeof RS.copyText === 'function' ? RS.copyText(full) : Promise.resolve(false);
        Promise.resolve(pending).then(function(ok) {
            if (!ok) {
                if (typeof showToast === 'function') showToast('Could not copy ' + lowerFirst(noun), 'toast-error', 3000);
                return;
            }
            if (typeof showCopyConfirmationToast === 'function') showCopyConfirmationToast(noun);
            var button = wrap.querySelector('.ui-hash-btn');
            if (!button) return;
            button.innerHTML = icon('check');
            button.classList.add('is-done');
            clearTimeout(button._rsCopiedTimer);
            button._rsCopiedTimer = setTimeout(function() {
                button.innerHTML = icon('copy');
                button.classList.remove('is-done');
            }, 2000);
        });
    }

    // Fit: the longest of full, 8…8, and 8…4 that fits the width the layout already gives it.
    var FIT = '.ui-hash-copy[data-hash-form="fit"]';
    var fitted = new Set();

    function fitHash(wrap) {
        var value = wrap.firstElementChild;
        if (!value || !wrap.clientWidth) return;
        var full = wrap.getAttribute('data-hash');
        var forms = [hash.text(full), hash.mid(full), hash.short(full)];
        for (var i = 0; i < forms.length; i++) {
            if (value.textContent !== forms[i]) value.textContent = forms[i];
            if (value.scrollWidth <= value.clientWidth + 0.5) return;
        }
    }

    var resizeObserver = typeof ResizeObserver === 'function' ? new ResizeObserver(function(entries) {
        entries.forEach(function(entry) {
            if (!entry.target.isConnected) release(entry.target);
            else fitHash(entry.target);
        });
    }) : null;

    function release(wrap) {
        fitted.delete(wrap);
        if (resizeObserver) resizeObserver.unobserve(wrap);
    }

    function watch(wrap) {
        if (fitted.has(wrap)) return;
        fitted.add(wrap);
        if (resizeObserver) resizeObserver.observe(wrap);
        else fitHash(wrap);
    }

    function refitAll() {
        fitted.forEach(function(wrap) {
            if (!wrap.isConnected) release(wrap);
            else fitHash(wrap);
        });
    }

    function watchAdded(records) {
        var added = false;
        records.forEach(function(record) {
            Array.prototype.forEach.call(record.addedNodes, function(node) {
                if (node.nodeType !== 1) return;
                if (node.matches(FIT)) { watch(node); added = true; }
                if (node.firstElementChild) {
                    Array.prototype.forEach.call(node.querySelectorAll(FIT), function(wrap) { watch(wrap); added = true; });
                }
            });
        });
        // Wraps removed since the last pass are released here, so the set never outgrows the screen.
        if (added) fitted.forEach(function(wrap) { if (!wrap.isConnected) release(wrap); });
    }

    function start() {
        Array.prototype.forEach.call(document.querySelectorAll(FIT), watch);
        if (typeof MutationObserver === 'function') {
            new MutationObserver(watchAdded).observe(document.body, { childList: true, subtree: true });
        }
        window.addEventListener('ratspeak-text-scale-changed', refitAll);
        if (document.fonts && document.fonts.ready) document.fonts.ready.then(refitAll);
    }

    document.addEventListener('click', function(event) {
        var target = event.target && event.target.closest &&
            event.target.closest('.ui-hash-copy > .ui-hash, .ui-hash-copy > .ui-hash-btn');
        if (target) copyHash(target.parentNode);
    });

    if (document.body) start();
    else document.addEventListener('DOMContentLoaded', start);

    // The mark after a Ratspeak user's name, at the name's size.
    function mark() {
        return '<span class="ui-name-mark" role="img" aria-label="Ratspeak user" title="Ratspeak user">' + icon('ratspeak') + '</span>';
    }

    function compact() {
        return typeof window.matchMedia === 'function' && window.matchMedia('(max-width: 768px)').matches;
    }

    var FOCUSABLE = 'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';
    var dialogSeq = 0;

    // A dialog on desktop and a bottom sheet on a phone. opts: { title, size: 'md' | 'lg', className,
    // body, foot, sheet (markup in place of body and foot), onClose(value), focus: selector (false leaves
    // focus, and a phone's keyboard, where it is), locked (no close button; only its own actions close it) }.
    // Returns { modal, dialog, body, foot, close(value), q(selector) }.
    function dialog(opts) {
        opts = opts || {};
        var titleId = 'ui-dialog-title-' + (++dialogSeq);
        var previous = document.activeElement;
        var modal = document.createElement('div');
        // rs-sheet-shell + open: native Back closes the top sheet through _ratspeakDismiss.
        modal.className = 'ui-modal rs-sheet-shell open';
        modal.setAttribute('role', 'presentation');
        modal.innerHTML = '<div class="ui-dialog' + (opts.size ? ' ui-dialog--' + esc(opts.size) : '') + (opts.className ? ' ' + esc(opts.className) : '') +
            '" role="dialog" aria-modal="true" aria-labelledby="' + titleId + '" tabindex="-1">' +
            '<div class="ui-dialog-handle" aria-hidden="true"></div>' +
            '<div class="ui-dialog-head">' + (opts.head || '<h2 class="ui-dialog-title">' + esc(opts.title || '') + '</h2>') +
                (opts.locked ? '' : '<button type="button" class="ui-icon-btn" data-ui-close aria-label="Close" title="Close">' + icon('x') + '</button>') + '</div>' +
            (opts.sheet != null ? opts.sheet
                : '<div class="ui-dialog-body">' + (opts.body || '') + '</div>' + (opts.foot ? '<div class="ui-dialog-foot">' + opts.foot + '</div>' : '')) +
            '</div>';
        var box = modal.firstElementChild;
        var heading = box.querySelector('.ui-dialog-head h2, .ui-dialog-head .ui-dialog-title');
        if (heading) heading.id = titleId;
        var closed = false;

        function onKey(e) {
            if (e.key === 'Escape') {
                e.preventDefault();
                e.stopPropagation();
                if (!opts.locked) close(null);
            } else if (e.key === 'Tab') {
                var items = Array.prototype.filter.call(box.querySelectorAll(FOCUSABLE), function(el) { return el.offsetParent !== null; });
                if (!items.length) return;
                var first = items[0], last = items[items.length - 1];
                if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
                else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
            }
        }

        function close(value) {
            if (closed) return;
            closed = true;
            modal.removeEventListener('keydown', onKey);
            // It fades out (.is-closing) and then leaves the page; native Back no longer sees it.
            modal.classList.remove('open');
            modal.classList.add('is-closing');
            setTimeout(function() { if (modal.parentNode) modal.parentNode.removeChild(modal); }, 200);
            if (previous && typeof previous.focus === 'function' && document.contains(previous)) {
                try { previous.focus({ preventScroll: true }); } catch (_) {}
            }
            if (typeof opts.onClose === 'function') opts.onClose(value);
        }

        modal.addEventListener('keydown', onKey);
        modal.addEventListener('click', function(e) {
            if (e.target.closest && e.target.closest('[data-ui-close]')) close(null);
            else if (e.target === modal && !opts.locked) close(null);
        });
        modal._ratspeakDismiss = function() { if (!opts.locked) close(null); return true; };
        document.body.appendChild(modal);
        if (!opts.locked && compact() && RS.gestures && typeof RS.gestures.attachDragDismiss === 'function') {
            RS.gestures.attachDragDismiss(box, {
                axis: 'y',
                blockIfScrolled: true,
                skipIf: function(e) { return !!(e.target.closest && e.target.closest('button, input, textarea, select, a, video')); },
                onCommit: function() { close(null); }
            });
        }
        if (opts.focus !== false) {
            var target = (opts.focus && box.querySelector(opts.focus)) || (!compact() && box.querySelector('input:not([type="checkbox"]):not([type="radio"]), textarea')) || box;
            try { target.focus({ preventScroll: true }); } catch (_) {}
        }
        return {
            modal: modal,
            dialog: box,
            body: box.querySelector('.ui-dialog-body'),
            foot: box.querySelector('.ui-dialog-foot'),
            close: close,
            isOpen: function() { return !closed; },
            q: function(selector) { return box.querySelector(selector); }
        };
    }

    // Detail header: back (phone), one identity button (avatar or tile, name, one status line), trailing icon buttons.
    // o: { back (label), lead, name | nameHtml, sub (html), label (aria), actions, after, rule, id, cls }.
    function detailHead(o) {
        o = o || {};
        return '<header class="ui-detail-head' + (o.rule ? ' ui-detail-head--rule' : '') + (o.cls ? ' ' + esc(o.cls) : '') + '"' + (o.id ? ' id="' + esc(o.id) + '"' : '') + '>' +
            (o.back ? '<button type="button" class="ui-icon-btn ui-phone-only" aria-label="' + esc(o.back) + '" title="Back">' + icon('chevron-left') + '</button>' : '') +
            '<button type="button" class="ui-who" aria-label="' + esc(o.label || o.name || 'Details') + '">' + (o.lead || '') +
                '<span class="ui-who-text"><span class="ui-who-name">' + (o.nameHtml || esc(o.name)) + '</span>' +
                (o.sub ? '<span class="ui-who-sub">' + o.sub + '</span>' : '') + '</span></button>' +
            (o.actions ? '<div class="ui-pane-actions">' + o.actions + '</div>' : '') + (o.after || '') +
        '</header>';
    }

    function glyph(value) { return /^\s*</.test(value || '') ? value : icon(value); }

    // A menu under its trigger on desktop, a sheet on a phone. opts: { title (the sheet's head and the
    // menu's name), label (its name when there is no title), caption, items, align: 'end' (default) or
    // 'start', placement: 'top', rect (an anchor box in place of the trigger's), sheet (true or false
    // forces one), hints (show hints on desktop too), keepFocus (leave focus, and a phone's keyboard,
    // where it is), cls, onClose(item | null) }.
    // Item: { label, hint, icon (glyph name or svg), checked (radio items), danger, disabled, onSelect }
    // or { separator: true }. Returns { trigger, el, close() }, or null when a second press closed it.
    var openMenu = null;
    function closeMenu() {
        if (openMenu) openMenu.close(null);
    }

    function menuItemsHtml(items, hints) {
        return items.map(function(item, index) {
            if (item.separator) return '<div class="ui-menu-sep" role="separator"></div>';
            var radio = Object.prototype.hasOwnProperty.call(item, 'checked');
            return '<button type="button" class="ui-menu-item' + (item.danger ? ' ui-menu-item--danger' : '') + '" role="' + (radio ? 'menuitemradio' : 'menuitem') + '"' +
                (radio ? ' aria-checked="' + (item.checked ? 'true' : 'false') + '"' : '') + (item.disabled ? ' disabled' : '') + ' data-ui-menu-index="' + index + '">' +
                (item.icon ? glyph(item.icon) : '') + '<span class="lbl">' + esc(item.label) + (hints && item.hint ? '<small>' + esc(item.hint) + '</small>' : '') + '</span>' +
                (radio && item.checked ? icon('check', 'check') : '') + '</button>';
        }).join('');
    }

    function placeMenu(pop, rect, opts) {
        var safe = typeof safeAreaInsets === 'function' ? safeAreaInsets() : { top: 0, right: 0, bottom: 0, left: 0 };
        var m = 8, w = pop.offsetWidth, h = pop.offsetHeight;
        var minX = safe.left + m, maxX = window.innerWidth - safe.right - m;
        var minY = safe.top + m, maxY = window.innerHeight - safe.bottom - m;
        var start = opts.align === 'start';
        var left = Math.max(minX, Math.min(start ? rect.left : rect.right - w, maxX - w));
        var above = rect.top - 4 - h, below = rect.bottom + 4;
        var up = opts.placement === 'top' ? above >= minY : (below + h > maxY && above >= minY);
        pop.style.left = Math.round(left) + 'px';
        pop.style.top = Math.round(Math.max(minY, Math.min(up ? above : below, maxY - h))) + 'px';
    }

    function menu(trigger, opts) {
        opts = opts || {};
        var items = opts.items || [];
        // A second press on the same trigger closes its menu.
        if (openMenu && trigger && openMenu.trigger === trigger) {
            closeMenu();
            return null;
        }
        closeMenu();
        var useSheet = opts.sheet != null ? !!opts.sheet : compact();
        var name = opts.title || opts.label || '';
        var caption = opts.caption ? '<div class="ui-menu-caption">' + esc(opts.caption) + '</div>' : '';
        var handle = { trigger: trigger, closed: false };
        var expands = !!(trigger && trigger.setAttribute);
        function choose(index) {
            var item = items[index];
            if (!item || item.disabled || item.separator) return;
            handle.close(item);
            if (typeof item.onSelect === 'function') item.onSelect();
        }
        function finish(value) {
            if (openMenu === handle) openMenu = null;
            if (expands) trigger.setAttribute('aria-expanded', 'false');
            if (typeof opts.onClose === 'function') opts.onClose(value || null);
        }
        if (expands) trigger.setAttribute('aria-expanded', 'true');
        if (useSheet) {
            var sheet = dialog({
                title: opts.title || '',
                className: opts.cls,
                focus: opts.keepFocus ? false : null,
                sheet: '<div class="ui-menu ui-menu--sheet" role="menu"' + (name ? ' aria-label="' + esc(name) + '"' : '') + '>' + caption + menuItemsHtml(items, true) + '</div>',
                onClose: function(value) { handle.closed = true; finish(value); }
            });
            sheet.dialog.addEventListener('mousedown', function(e) { if (opts.keepFocus) e.preventDefault(); });
            sheet.dialog.addEventListener('click', function(e) {
                var btn = e.target.closest && e.target.closest('[data-ui-menu-index]');
                if (btn) choose(Number(btn.getAttribute('data-ui-menu-index')));
            });
            handle.el = sheet.dialog;
            handle.close = function(value) { sheet.close(value); };
            openMenu = handle;
            return handle;
        }
        var pop = document.createElement('div');
        pop.className = 'ui-menu ui-pop' + (opts.cls ? ' ' + opts.cls : '');
        pop.setAttribute('role', 'menu');
        if (name) pop.setAttribute('aria-label', name);
        pop.innerHTML = caption + menuItemsHtml(items, !!opts.hints);
        document.body.appendChild(pop);
        placeMenu(pop, opts.rect || trigger.getBoundingClientRect(), opts);
        function reflow() { if (!handle.closed && !opts.rect && trigger.isConnected) placeMenu(pop, trigger.getBoundingClientRect(), opts); }
        function outside(e) {
            if (pop.contains(e.target) || (trigger && trigger.contains && trigger.contains(e.target))) return;
            handle.close(null);
        }
        function onKey(e) {
            var buttons = Array.prototype.slice.call(pop.querySelectorAll('.ui-menu-item:not([disabled])'));
            var at = buttons.indexOf(document.activeElement);
            if (e.key === 'Escape') {
                e.preventDefault();
                e.stopPropagation();
                handle.close(null, true);
            } else if (e.key === 'Tab') {
                handle.close(null);
            } else if (/^(ArrowDown|ArrowUp|Home|End)$/.test(e.key) && buttons.length) {
                e.preventDefault();
                var next = e.key === 'Home' ? 0 : e.key === 'End' ? buttons.length - 1 :
                    at === -1 && e.key === 'ArrowUp' ? buttons.length - 1 :
                    (at + (e.key === 'ArrowDown' ? 1 : -1) + buttons.length) % buttons.length;
                buttons[next].focus();
            }
        }
        pop.addEventListener('mousedown', function(e) { if (opts.keepFocus) e.preventDefault(); });
        pop.addEventListener('click', function(e) {
            var btn = e.target.closest && e.target.closest('[data-ui-menu-index]');
            if (btn) choose(Number(btn.getAttribute('data-ui-menu-index')));
        });
        document.addEventListener('pointerdown', outside, true);
        document.addEventListener('keydown', onKey, true);
        window.addEventListener('resize', reflow);
        window.addEventListener('scroll', reflow, true);
        handle.el = pop;
        handle.close = function(value, returnFocus) {
            if (handle.closed) return;
            handle.closed = true;
            document.removeEventListener('pointerdown', outside, true);
            document.removeEventListener('keydown', onKey, true);
            window.removeEventListener('resize', reflow);
            window.removeEventListener('scroll', reflow, true);
            var hadFocus = pop.contains(document.activeElement);
            if (pop.parentNode) pop.parentNode.removeChild(pop);
            if ((hadFocus || returnFocus) && trigger && trigger.focus && trigger.isConnected) {
                try { trigger.focus({ preventScroll: true }); } catch (_) { trigger.focus(); }
            }
            finish(value);
        };
        openMenu = handle;
        if (!opts.keepFocus) {
            var first = pop.querySelector('.ui-menu-item[aria-checked="true"], .ui-menu-item:not([disabled])');
            if (first) try { first.focus({ preventScroll: true }); } catch (_) { first.focus(); }
        }
        return handle;
    }

    kit.esc = esc;
    kit.mark = mark;
    kit.compact = compact;
    kit.dialog = dialog;
    kit.menu = menu;
    kit.closeMenu = closeMenu;
    kit.detailHead = detailHead;
    kit.icon = icon;
    kit.icons = Object.keys(GLYPHS);
    kit.time = time;
    kit.presenceFor = presenceFor;
    kit.avatar = avatar;
    kit.hash = hash;
})();
