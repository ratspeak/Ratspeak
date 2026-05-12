var activityLog = [];
var activityEnabled = false;
var activityLevel = 'standard';
var activityFilters = {
    all: true,
    announce: true,
    path: true,
    message: true,
    lxst: true,
    interface: true,
    link: true,
    error: true
};

var ACTIVITY_MAX_ENTRIES = 500;

// Auto-scroll new entries only when pinned to bottom; 8px tolerance for sub-pixel rounding.
var activityStickToBottom = true;
var ACTIVITY_STICK_TOLERANCE_PX = 8;

var ACTIVITY_FILTER_LABELS = {
    all: 'All',
    announce: 'Announces',
    path: 'Paths',
    message: 'Messages',
    lxst: 'LXST',
    interface: 'Interfaces',
    link: 'Links',
    error: 'Errors'
};

// essential < standard < detailed
var LEVEL_HIERARCHY = { essential: 0, standard: 1, detailed: 2 };

var LEVEL_TYPES = {
    essential: ['error'],
    standard: ['error', 'message', 'lxst', 'interface', 'link'],
    detailed: ['error', 'message', 'lxst', 'interface', 'link', 'announce', 'path']
};

function initActivity() {
    localStorage.removeItem('rs-activity-enabled');
    var storedLevel = localStorage.getItem('rs-activity-level');
    if (storedLevel && LEVEL_HIERARCHY[storedLevel] !== undefined) {
        activityLevel = storedLevel;
    }

    updateActivityUI();
    renderActivityFilters();

    var enableBtn = document.getElementById('activity-enable-btn');
    if (enableBtn) {
        enableBtn.addEventListener('click', function() {
            toggleActivityEnabled(true);
        });
    }

    var toggle = document.getElementById('activity-enabled-toggle');
    if (toggle) {
        toggle.addEventListener('change', function() {
            toggleActivityEnabled(this.checked);
        });
    }

    var levelBtns = document.querySelectorAll('.activity-level-btn');
    levelBtns.forEach(function(btn) {
        btn.addEventListener('click', function() {
            setActivityLevel(this.getAttribute('data-level'));
        });
    });

    var clearBtn = document.getElementById('activity-clear-btn');
    if (clearBtn) {
        clearBtn.addEventListener('click', function() {
            activityLog = [];
            activityStickToBottom = true;
            renderActivityFeed();
        });
    }

    var feed = document.getElementById('activity-feed');
    if (feed) {
        feed.addEventListener('scroll', function() {
            var distanceFromBottom = feed.scrollHeight - feed.scrollTop - feed.clientHeight;
            activityStickToBottom = distanceFromBottom <= ACTIVITY_STICK_TOLERANCE_PX;
        }, { passive: true });
    }

    RS.listen('network_event', function(data) {
        if (!activityEnabled) return;
        addActivityEntry(data);
    });

    RS.listen('network_log_level_changed', function(data) {
        if (data && data.level) {
            activityLevel = data.level;
            localStorage.setItem('rs-activity-level', activityLevel);
            updateLevelButtons();
        }
        if (data && data.restart_required) {
            showToast('Log level updated. Restart required to take effect', 'toast-orange', 5000);
        }
    });

    RS.invoke('enable_network_log', { args: { enabled: false, level: activityLevel } }).catch(function() {});
}

function toggleActivityEnabled(enabled) {
    activityEnabled = enabled;
    localStorage.removeItem('rs-activity-enabled');
    RS.invoke('enable_network_log', { args: { enabled: enabled, level: activityLevel } }).catch(function() {});
    if (!enabled) {
        activityLog = [];
        activityStickToBottom = true;
        renderActivityFeed();
    }
    updateActivityUI();

    if (enabled) {
        showToast('Network logging enabled', 'toast-green', 2000);
    } else {
        showToast('Network logging disabled', 'toast-orange', 2000);
    }
}

function setActivityLevel(level) {
    if (!LEVEL_HIERARCHY.hasOwnProperty(level)) return;
    activityLevel = level;
    localStorage.setItem('rs-activity-level', level);
    RS.invoke('set_network_log_level', { level: level }).catch(function() {});
    updateLevelButtons();
    renderActivityFeed();
}

function updateActivityUI() {
    var gate = document.getElementById('activity-privacy-gate');
    var active = document.getElementById('activity-active');
    var clearBtn = document.getElementById('activity-clear-btn');
    var toggle = document.getElementById('activity-enabled-toggle');

    if (activityEnabled) {
        if (gate) gate.style.display = 'none';
        if (active) active.style.display = '';
        if (clearBtn) clearBtn.style.display = '';
        if (toggle) toggle.checked = true;
    } else {
        if (gate) gate.style.display = '';
        if (active) active.style.display = 'none';
        if (clearBtn) clearBtn.style.display = 'none';
        if (toggle) toggle.checked = false;
    }
    updateLevelButtons();
}

function updateLevelButtons() {
    var btns = document.querySelectorAll('.activity-level-btn');
    btns.forEach(function(btn) {
        if (btn.getAttribute('data-level') === activityLevel) {
            btn.classList.add('active');
        } else {
            btn.classList.remove('active');
        }
    });
}

function renderActivityFilters() {
    var container = document.getElementById('activity-filters');
    if (!container) return;

    var html = '';
    var types = ['all', 'announce', 'path', 'message', 'lxst', 'interface', 'link', 'error'];
    for (var i = 0; i < types.length; i++) {
        var type = types[i];
        var label = ACTIVITY_FILTER_LABELS[type];
        var isActive = activityFilters[type];
        html += '<button class="activity-filter-chip' + (isActive ? ' active' : '') + '" data-filter="' + type + '">' + label + '</button>';
    }
    container.innerHTML = html;

    container.querySelectorAll('.activity-filter-chip').forEach(function(chip) {
        chip.addEventListener('click', function() {
            toggleActivityFilter(this.getAttribute('data-filter'));
        });
    });
}

function toggleActivityFilter(type) {
    if (type === 'all') {
        var allOn = activityFilters.all;
        var keys = Object.keys(activityFilters);
        for (var i = 0; i < keys.length; i++) {
            activityFilters[keys[i]] = !allOn;
        }
    } else {
        activityFilters[type] = !activityFilters[type];
        var allSelected = true;
        var filterKeys = ['announce', 'path', 'message', 'lxst', 'interface', 'link', 'error'];
        for (var i = 0; i < filterKeys.length; i++) {
            if (!activityFilters[filterKeys[i]]) { allSelected = false; break; }
        }
        activityFilters.all = allSelected;
    }
    renderActivityFilters();
    renderActivityFeed();
}

function addActivityEntry(entry) {
    var item = {
        type: entry.type || 'interface',
        message: entry.message || '',
        detail: entry.detail || '',
        timestamp: entry.timestamp || Date.now(),
        level: entry.level || 'standard'
    };

    activityLog.push(item);
    if (activityLog.length > ACTIVITY_MAX_ENTRIES) {
        activityLog.shift();
    }

    if (isEntryVisible(item)) {
        appendActivityEntry(item);
    }
}

function isEntryVisible(entry) {
    if (!activityFilters.all && !activityFilters[entry.type]) return false;
    var entryRank = LEVEL_HIERARCHY[entry.level];
    if (entryRank === undefined) entryRank = 1;
    var configRank = LEVEL_HIERARCHY[activityLevel];
    if (configRank === undefined) configRank = 1;
    if (entryRank > configRank) return false;
    return true;
}

function appendActivityEntry(entry) {
    var feed = document.getElementById('activity-feed');
    if (!feed) return;

    var empty = feed.querySelector('.activity-empty');
    if (empty) empty.remove();

    var div = document.createElement('div');
    div.className = 'activity-entry';
    div.setAttribute('data-type', entry.type);

    var time = formatActivityTime(entry.timestamp);
    div.innerHTML =
        '<span class="activity-entry-time">' + time + '</span>' +
        '<span class="activity-entry-text">' + escapeHtml(entry.message) + '</span>' +
        (entry.detail ? '<span class="activity-entry-detail">' + escapeHtml(entry.detail) + '</span>' : '');

    feed.appendChild(div);

    if (activityStickToBottom) {
        feed.scrollTop = feed.scrollHeight;
    }
}

function renderActivityFeed() {
    var feed = document.getElementById('activity-feed');
    if (!feed) return;

    var filtered = activityLog.filter(isEntryVisible);

    if (filtered.length === 0) {
        feed.innerHTML = '<div class="activity-empty">Listening for network events...</div>';
        return;
    }

    var html = '';
    for (var i = 0; i < filtered.length; i++) {
        var entry = filtered[i];
        var time = formatActivityTime(entry.timestamp);
        html += '<div class="activity-entry" data-type="' + entry.type + '">' +
            '<span class="activity-entry-time">' + time + '</span>' +
            '<span class="activity-entry-text">' + escapeHtml(entry.message) + '</span>' +
            (entry.detail ? '<span class="activity-entry-detail">' + escapeHtml(entry.detail) + '</span>' : '') +
        '</div>';
    }
    feed.innerHTML = html;
    if (activityStickToBottom) {
        feed.scrollTop = feed.scrollHeight;
    }
}

function formatActivityTime(ts) {
    var d = new Date(typeof ts === 'number' ? ts : Date.parse(ts));
    if (isNaN(d.getTime())) return '--:--:--';
    var m = d.getMinutes().toString().padStart(2, '0');
    var s = d.getSeconds().toString().padStart(2, '0');
    if (_use12Hour) {
        var h = d.getHours();
        var period = h >= 12 ? 'PM' : 'AM';
        h = h % 12 || 12;
        return h + ':' + m + ':' + s + ' ' + period;
    }
    return d.getHours().toString().padStart(2, '0') + ':' + m + ':' + s;
}

var REASON_LABELS = {
    Manual: 'manual',
    Malformed: 'malformed',
    RateLimit: 'rate-limited',
    ProtocolViolation: 'protocol violation'
};

function fetchSystemDrops() {
    return RS.invoke('api_network_blackhole')
        .then(function(payload) { return (payload && payload.entries) || []; })
        .catch(function() { return []; });
}

function renderSystemDrops(entries) {
    var card = document.getElementById('system-drops-card');
    var summary = document.getElementById('system-drops-summary');
    var list = document.getElementById('system-drops-list');
    var purgeBtn = document.getElementById('system-drops-purge-unverified-btn');
    if (!card || !summary || !list) return;

    var allEntries = entries || [];
    var systemEntries = allEntries.filter(function(e) { return e.reason !== 'Manual'; });
    // Unverified manual entries \u2014 pre-fix garbage or identities pruned from
    // recent_announces. Count separately so the user has a clear path to clean
    // them up via "Purge unverified".
    var unverifiedManual = allEntries.filter(function(e) {
        return e.reason === 'Manual' && e.verified === false;
    });

    if (purgeBtn) {
        purgeBtn.style.display = unverifiedManual.length > 0 ? '' : 'none';
    }

    if (systemEntries.length === 0 && unverifiedManual.length === 0) {
        card.style.display = 'none';
        return;
    }

    var counts = {};
    systemEntries.forEach(function(e) {
        var label = REASON_LABELS[e.reason] || e.reason || 'unknown';
        counts[label] = (counts[label] || 0) + 1;
    });
    if (unverifiedManual.length > 0) {
        counts['unverified manual'] = unverifiedManual.length;
    }
    var summaryParts = Object.keys(counts).sort().map(function(k) { return counts[k] + ' ' + k; });
    var totalShown = systemEntries.length + unverifiedManual.length;
    summary.textContent = totalShown + ' \u00B7 ' + summaryParts.join(', ');

    var html = '';
    var renderRow = function(e, extraPill) {
        var hashShort = (e.hash || '').substring(0, 16);
        var label = REASON_LABELS[e.reason] || e.reason || 'unknown';
        var pillClass = 'system-drops-pill system-drops-pill-' + (e.reason || 'unknown').toLowerCase();
        var expiry;
        if (typeof e.expires_in === 'number') {
            expiry = e.expires_in > 0 ? formatExpiryShort(Math.floor(e.expires_in)) : 'expired';
        } else {
            expiry = 'no expiry';
        }
        return '<div class="system-drops-row">' +
            '<span class="system-drops-hash" title="' + escapeHtml(e.hash || '') + '">' + escapeHtml(hashShort) + '\u2026</span>' +
            '<span class="' + pillClass + '">' + escapeHtml(label) + '</span>' +
            (extraPill || '') +
            '<span class="system-drops-expiry">' + escapeHtml(expiry) + '</span>' +
        '</div>';
    };
    systemEntries.forEach(function(e) { html += renderRow(e); });
    unverifiedManual.forEach(function(e) {
        html += renderRow(e, '<span class="system-drops-pill system-drops-pill-unverified" title="No announce backs this identity">unverified</span>');
    });
    list.innerHTML = html;
    card.style.display = '';
}

function formatExpiryShort(sec) {
    if (sec >= 86400) return Math.floor(sec / 86400) + 'd';
    if (sec >= 3600) return Math.floor(sec / 3600) + 'h';
    if (sec >= 60) return Math.floor(sec / 60) + 'm';
    return sec + 's';
}

function refreshSystemDrops() {
    fetchSystemDrops().then(renderSystemDrops);
}

function initSystemDrops() {
    var header = document.querySelector('#system-drops-card .system-drops-header');
    var body = document.getElementById('system-drops-body');
    if (header && body) {
        var toggle = function() {
            var open = !body.hasAttribute('hidden');
            if (open) {
                body.setAttribute('hidden', '');
                header.setAttribute('aria-expanded', 'false');
            } else {
                body.removeAttribute('hidden');
                header.setAttribute('aria-expanded', 'true');
            }
        };
        header.addEventListener('click', toggle);
        header.addEventListener('keydown', function(e) {
            if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); toggle(); }
        });
    }

    var clearBtn = document.getElementById('system-drops-clear-btn');
    if (clearBtn) {
        clearBtn.addEventListener('click', function() {
            if (typeof rsConfirm !== 'function') {
                RS.invoke('clear_system_blackholes').catch(function() {});
                return;
            }
            rsConfirm({
                message: 'Clear all system-populated network drops? Manual blocks are not affected.',
                confirmText: 'Clear'
            }).then(function(ok) {
                if (!ok) return;
                RS.invoke('clear_system_blackholes').catch(function() {});
            });
        });
    }

    var purgeBtn = document.getElementById('system-drops-purge-unverified-btn');
    if (purgeBtn) {
        // Drops Manual blackhole entries whose identity is not currently
        // backed by a known announce. Useful after pre-fix builds populated
        // the table with LXMF-dest-hash bytes that can never match an
        // announcer. Also removes legit-but-unseen entries — warn the user.
        purgeBtn.addEventListener('click', function() {
            if (typeof rsConfirm !== 'function') return;
            rsConfirm({
                message: 'Remove network blocks with no recent announce evidence? This cleans up pre-fix garbage entries but may also drop blocks for contacts you have not heard from in a long time.',
                confirmText: 'Purge',
                danger: true
            }).then(function(ok) {
                if (!ok) return;
                RS.invoke('purge_unverified_blackholes').then(function(resp) {
                    if (typeof showToast !== 'function') return;
                    var n = (resp && resp.purged) | 0;
                    if (n === 0) {
                        showToast('Nothing to purge — all blocks are verified.', 'toast-info', 3000);
                    } else {
                        showToast('Purged ' + n + ' unverified entr' + (n === 1 ? 'y' : 'ies') + '.', 'toast-green', 3000);
                    }
                }).catch(function() {});
            });
        });
    }

    RS.listen('blackhole_update', refreshSystemDrops);
    refreshSystemDrops();
}

initSystemDrops();

initActivity();
