var badgeLabelMap = {
    'announce_summary': 'announce',
    'interface': 'network',
    'log': 'system',
    'status': 'status'
};

function renderLog() {
    var container = document.getElementById('eventlog-container');
    if (!container) return;

    var filtered = events;
    if (activeLogFilter !== 'all') {
        filtered = events.filter(function(e) {
            var cat = e.category || e.type || 'system';
            if (activeLogFilter === 'alert') return cat === 'alert';
            if (activeLogFilter === 'message') return cat === 'message';
            if (activeLogFilter === 'system') return cat === 'system' || cat === 'log' || cat === 'interface' || cat === 'path' || cat === 'link' || cat === 'status';
            return true;
        });
    }

    filtered = filtered.slice().reverse();

    if (filtered.length === 0) {
        container.innerHTML = '<div class="text-sm text-muted-color">No events matching filter.</div>';
        return;
    }

    container.innerHTML = filtered.map(function(e) {
        var time = formatTime(e.timestamp);
        var rel = RS.relativeTime(e.timestamp);
        var cat = e.category || e.type || 'system';
        var displayLabel = badgeLabelMap[cat] || cat;

        var msgHtml;
        if (cat === 'status' && e.online !== undefined) {
            msgHtml = '<span class="log-status-dots">' +
                '<span class="log-status-count online"><span class="log-status-pip"></span>' + e.online + ' online</span>' +
                '<span class="log-status-sep">&middot;</span>' +
                '<span class="log-status-count stale"><span class="log-status-pip"></span>' + e.stale + ' stale</span>' +
                '<span class="log-status-sep">&middot;</span>' +
                '<span class="log-status-count offline"><span class="log-status-pip"></span>' + e.offline + ' offline</span>' +
            '</span>';
        } else {
            msgHtml = escapeHtml(e.message);
        }

        var mobile = typeof isMobile === 'function' && isMobile();
        var displayTime = mobile ? rel : time;
        var tooltipTime = mobile ? time : rel;

        return '<div class="log-entry">' +
            '<span class="log-time" title="' + tooltipTime + '">' + displayTime + '</span>' +
            '<span class="log-badge ' + cat + '">' + displayLabel + '</span>' +
            '<span class="log-msg">' + msgHtml + '</span>' +
        '</div>';
    }).join('');

    container.scrollTop = 0;
}

// Hook extended by health.js for interface cards, path table, etc.
function renderStats(data) {
}

function renderCockpitEvents() {
    var container = document.getElementById('cockpit-events');
    if (!container) return;

    var recent = events.slice().reverse().slice(0, 12);

    if (recent.length === 0) {
        container.innerHTML = '<div class="inline-hint" style="padding:8px 0;">Waiting for events...</div>';
        return;
    }

    container.innerHTML = recent.map(function(e) {
        var rel = RS.relativeTime(e.timestamp);
        var cat = e.category || e.type || 'system';
        var displayLabel = badgeLabelMap[cat] || cat;
        var msg = e.message || '';
        if (msg.length > 60) msg = msg.substring(0, 57) + '...';

        return '<div class="log-entry">' +
            '<span class="log-time">' + rel + '</span>' +
            '<span class="log-badge ' + cat + '">' + displayLabel + '</span>' +
            '<span class="log-msg">' + escapeHtml(msg) + '</span>' +
        '</div>';
    }).join('');
}

document.querySelectorAll('#view-eventlog .log-filter').forEach(function(btn) {
    btn.addEventListener('click', function() {
        document.querySelectorAll('#view-eventlog .log-filter').forEach(function(b) { b.classList.remove('active'); });
        this.classList.add('active');
        activeLogFilter = this.dataset.filter;
        renderLog();
    });
});
