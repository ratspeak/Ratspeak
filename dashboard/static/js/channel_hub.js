// Desktop RRC hub hosting. Client-side Channels traffic remains in channels.js;
// this file owns only the operator surface and its stable IPC read model.

var channelHubOverview = null;
var _channelHubOverviewLoadedAt = 0;
var _channelHubOverviewPromise = null;
var _channelHubStatusRenderer = null;
var _channelHubManagerSequence = 0;
var _channelHubManagerDismiss = null;
var _channelHubIdentityGeneration = 0;
var _channelHubHomeBusy = false;

function _channelHubPlural(count, singular, plural) {
    return count + ' ' + (count === 1 ? singular : (plural || singular + 's'));
}

function _channelHubStatusModel(overview) {
    overview = overview || {};
    var settings = overview.settings || {};
    var status = overview.status || {};
    if (status.running) {
        return {
            label: 'Hosting',
            detail: _channelHubPlural(Number(status.welcomed_sessions) || 0, 'person', 'people') +
                ' here · ' + _channelHubPlural(Number(status.registered_rooms) || 0, 'channel'),
            tone: status.registry_degraded ? 'warning' : 'online',
            action: 'stop',
            actionLabel: 'Stop hosting'
        };
    }
    if (settings.enabled) {
        return {
            label: 'Waiting for network',
            detail: 'Hosting will begin when the network is ready',
            tone: 'warning',
            action: 'stop',
            actionLabel: 'Turn off'
        };
    }
    return {
        label: 'Not running',
        detail: 'Create a place for your community',
        tone: 'offline',
        action: 'start',
        actionLabel: 'Start hosting'
    };
}

function _channelHubAnnounceLabel(seconds) {
    var value = Number(seconds) || 0;
    if (value === 0) return 'When started';
    if (value < 3600) return 'Every ' + Math.round(value / 60) + ' min';
    if (value === 3600) return 'Every hour';
    if (value < 86400) return 'Every ' + Math.round(value / 3600) + ' hours';
    return 'Every day';
}

function channelHubOwnDestinationHash() {
    if (!channelHubOverview) return '';
    var status = channelHubOverview.status || {};
    return String(status.destination_hash || channelHubOverview.destination_hash || '').toLowerCase();
}

function _channelHubHasOwnedHub(overview) {
    if (!overview || !overview.supported) return false;
    return !!(overview.created ||
        (overview.settings && overview.settings.enabled) ||
        (overview.status && overview.status.running));
}

function _channelHubCurrentDestination() {
    if (typeof channelsSnapshot === 'undefined' || !channelsSnapshot.hub) return '';
    return String(channelsSnapshot.hub.destination_hash || '').toLowerCase();
}

function channelHubRenderHome(overview) {
    overview = overview || channelHubOverview;
    var section = document.getElementById('channel-owned-hub');
    if (!section) return;
    var visible = _channelHubHasOwnedHub(overview);
    section.hidden = !visible;
    if (!visible) return;

    var settings = overview.settings || {};
    var status = overview.status || {};
    var model = _channelHubStatusModel(overview);
    var destination = channelHubOwnDestinationHash();
    var current = !!destination && _channelHubCurrentDestination() === destination;
    var connected = current && typeof _channelsIsConnected === 'function' && _channelsIsConnected();
    var connecting = current && typeof _channelsIsConnecting === 'function' && _channelsIsConnecting();
    var counts = _channelHubPlural(Number(status.welcomed_sessions) || 0, 'person', 'people') +
        ' · ' + _channelHubPlural(Number(status.registered_rooms) || 0, 'channel');
    var statusText = model.label;
    if (status.running) {
        statusText = connected ? 'Connected · ' + counts : (connecting ? 'Connecting… · Hosting' : 'Hosting · ' + counts);
    }

    var card = document.getElementById('channel-owned-hub-card');
    var name = document.getElementById('channel-owned-hub-name');
    var meta = document.getElementById('channel-owned-hub-status');
    var open = document.getElementById('channel-owned-hub-open');
    var manage = document.getElementById('channel-owned-hub-manage');
    var hubName = settings.hub_name || status.hub_name || 'Ratspeak Hub';
    if (name) name.textContent = hubName;
    if (meta) meta.textContent = statusText;
    if (card) {
        card.dataset.tone = status.registry_degraded ? 'warning' : model.tone;
        card.dataset.current = current ? 'true' : 'false';
    }
    if (open) {
        open.disabled = _channelHubHomeBusy;
        open.setAttribute('aria-label', status.running
            ? (connected ? 'Open your hub' : 'Connect to ' + hubName)
            : 'Manage ' + hubName);
        open.title = status.running ? (connected ? 'Open your hub' : 'Connect to your hub') : 'Manage your hub';
    }
    if (manage) {
        manage.disabled = _channelHubHomeBusy;
        manage.setAttribute('aria-label', 'Manage ' + hubName);
        manage.title = 'Manage ' + hubName;
    }
}

function _channelHubApplyOverview(overview) {
    if (!overview) return channelHubOverview;
    channelHubOverview = overview;
    _channelHubOverviewLoadedAt = Date.now();
    if (_channelHubStatusRenderer) _channelHubStatusRenderer(overview);
    channelHubRenderHome(overview);
    return overview;
}

function channelHubLoad(force) {
    var now = Date.now();
    if (!force && channelHubOverview && now - _channelHubOverviewLoadedAt < 2000) {
        return Promise.resolve(channelHubOverview);
    }
    if (_channelHubOverviewPromise) return _channelHubOverviewPromise;
    var identityGeneration = _channelHubIdentityGeneration;
    var pending = RS.invoke('api_channel_hub').then(function(overview) {
        if (identityGeneration !== _channelHubIdentityGeneration) return null;
        return _channelHubApplyOverview(overview);
    });
    _channelHubOverviewPromise = pending;
    pending.then(function() {
        if (_channelHubOverviewPromise === pending) _channelHubOverviewPromise = null;
    }, function() {
        if (_channelHubOverviewPromise === pending) _channelHubOverviewPromise = null;
    });
    return pending;
}

function _channelHubIcon(kind) {
    if (kind === 'join') {
        return '<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="M8.5 16.5a6 6 0 0 1 0-9"/><path d="M15.5 7.5a6 6 0 0 1 0 9"/><circle cx="12" cy="12" r="1.7" fill="currentColor" stroke="none"/></svg>';
    }
    return '<svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="1.6" fill="currentColor" stroke="none"/><path d="M8.7 8.7a4.7 4.7 0 0 0 0 6.6M15.3 15.3a4.7 4.7 0 0 0 0-6.6"/><path d="M5.3 5.3a9.5 9.5 0 0 0 0 13.4M18.7 18.7a9.5 9.5 0 0 0 0-13.4"/></svg>';
}

function _channelHubChoice(kind, titleText, detailText, statusText) {
    var choice = document.createElement('button');
    choice.type = 'button';
    choice.className = 'channel-hub-choice';
    choice.dataset.choice = kind;

    var icon = document.createElement('span');
    icon.className = 'channel-hub-choice-icon';
    icon.innerHTML = _channelHubIcon(kind);
    var copy = document.createElement('span');
    copy.className = 'channel-hub-choice-copy';
    var title = document.createElement('strong');
    title.textContent = titleText;
    var detail = document.createElement('span');
    detail.textContent = detailText;
    copy.appendChild(title);
    copy.appendChild(detail);
    choice.appendChild(icon);
    choice.appendChild(copy);
    if (statusText) {
        var status = document.createElement('span');
        status.className = 'channel-hub-choice-status';
        status.textContent = statusText;
        choice.appendChild(status);
    }
    var arrow = document.createElement('span');
    arrow.className = 'channel-hub-choice-arrow';
    arrow.setAttribute('aria-hidden', 'true');
    arrow.innerHTML = '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="9 18 15 12 9 6"/></svg>';
    choice.appendChild(arrow);
    return choice;
}

function channelsOpenAddSheet() {
    if (typeof _rsBuildSheet !== 'function') return;
    channelHubLoad(true).then(function(overview) {
        if (!overview) return;
        if (!overview.supported) {
            channelsOpenConnectSheet();
            return;
        }
        var built = _rsBuildSheet({ title: 'Channels' }, function() {});
        built.sheet.classList.add('channel-hub-launch-sheet');

        var intro = document.createElement('p');
        intro.className = 'channel-sheet-copy';
        intro.textContent = 'Join a conversation or make a place of your own.';
        built.body.appendChild(intro);

        var join = _channelHubChoice(
            'join',
            'Join a hub',
            'Find a nearby conversation or enter an address'
        );
        join.addEventListener('click', function() {
            built.dismiss();
            setTimeout(function() { channelsOpenConnectSheet(); }, 220);
        });
        built.body.appendChild(join);

        var model = _channelHubStatusModel(overview);
        var host = _channelHubChoice(
            'host',
            overview.created || (overview.settings && overview.settings.enabled) ? 'Manage your hub' : 'Host your own',
            model.detail,
            model.label
        );
        host.addEventListener('click', function() {
            built.dismiss();
            setTimeout(function() { channelHubOpenManager(overview); }, 220);
        });
        built.body.appendChild(host);

        var cancel = document.createElement('button');
        cancel.type = 'button';
        cancel.className = 'nr-btn nr-btn-secondary';
        cancel.textContent = 'Cancel';
        cancel.addEventListener('click', function() { built.dismiss(); });
        built.footer.appendChild(cancel);
        _channelsPresentSheet(built, join);
    }).catch(function(error) {
        if (typeof showToast === 'function') {
            showToast((error && error.message) || 'Hub hosting is unavailable', 'toast-orange', 3000);
        }
        channelsOpenConnectSheet();
    });
}

function _channelHubField(labelText, control, hintText) {
    var field = document.createElement('div');
    field.className = 'channel-host-field';
    var label = document.createElement('label');
    var id = 'channel-host-field-' + (++_channelsFieldSeq);
    control.id = id;
    label.htmlFor = id;
    label.textContent = labelText;
    field.appendChild(label);
    field.appendChild(control);
    if (hintText) {
        var hint = document.createElement('span');
        hint.className = 'channel-host-field-hint';
        hint.textContent = hintText;
        field.appendChild(hint);
    }
    return field;
}

function _channelHubToggle(labelText, detailText, checked) {
    var row = document.createElement('div');
    row.className = 'channel-host-toggle-row';
    var copy = document.createElement('span');
    copy.className = 'channel-host-toggle-copy';
    var label = document.createElement('strong');
    label.textContent = labelText;
    var detail = document.createElement('span');
    detail.textContent = detailText;
    copy.appendChild(label);
    copy.appendChild(detail);
    var toggle = document.createElement('label');
    toggle.className = 'prop-toggle';
    toggle.setAttribute('aria-label', labelText);
    var input = document.createElement('input');
    input.type = 'checkbox';
    input.checked = !!checked;
    var slider = document.createElement('span');
    slider.className = 'prop-slider';
    toggle.appendChild(input);
    toggle.appendChild(slider);
    row.appendChild(copy);
    row.appendChild(toggle);
    return { row: row, input: input };
}

function _channelHubConfigArgs(nameInput, greetingInput, announceInput, sendInput, acceptInput) {
    return {
        hub_name: nameInput.value.trim(),
        greeting: greetingInput.value.trim(),
        announce_interval_secs: Number(announceInput.value) || 0,
        resource_send: !!sendInput.checked,
        resource_accept: !!acceptInput.checked
    };
}

function _channelHubSettingsEqual(settings, args) {
    settings = settings || {};
    return String(settings.hub_name || '') === args.hub_name &&
        String(settings.greeting || '') === args.greeting &&
        Number(settings.announce_interval_secs || 0) === args.announce_interval_secs &&
        !!settings.resource_send_enabled === args.resource_send &&
        !!settings.resource_accept_enabled === args.resource_accept;
}

function _channelHubAdminNode(tagName, className, text) {
    var node = document.createElement(tagName);
    if (className) node.className = className;
    if (text !== undefined && text !== null) node.textContent = String(text);
    return node;
}

function _channelHubAdminShortIdentity(value) {
    value = String(value || '').toLowerCase();
    if (value.length <= 14) return value || 'Unknown identity';
    return value.slice(0, 8) + '\u2026' + value.slice(-4);
}

function _channelHubAdminDuration(seconds) {
    seconds = Math.max(0, Number(seconds) || 0);
    if (seconds < 60) return Math.round(seconds) + ' sec';
    if (seconds < 3600) return Math.round(seconds / 60) + ' min';
    if (seconds < 86400) return Math.round(seconds / 3600) + ' hr';
    return Math.round(seconds / 86400) + ' days';
}

function _channelHubAdminBytes(bytes) {
    bytes = Math.max(0, Number(bytes) || 0);
    if (bytes < 1024) return bytes + ' B';
    if (bytes < 1024 * 1024) return Math.round(bytes / 1024) + ' KiB';
    return Math.round(bytes / (1024 * 1024)) + ' MiB';
}

function _channelHubAdminDate(timestampMs) {
    var value = Number(timestampMs);
    if (!Number.isFinite(value) || value <= 0) return null;
    var date = new Date(value);
    return Number.isNaN(date.getTime()) ? null : date;
}

function _channelHubAdminGeneratedLabel(admin) {
    var generated = _channelHubAdminDate(admin && admin.generated_at_ms);
    if (!generated) return 'Local owner snapshot';
    return 'Updated ' + generated.toLocaleTimeString([], {
        hour: 'numeric',
        minute: '2-digit'
    });
}

function _channelHubAdminModeLabels(room) {
    var modes = room && room.modes || {};
    var labels = [];
    labels.push(room && room.registered ? 'Registered' : 'Session-only');
    if (modes.private) labels.push('Private');
    if (modes.invite_only) labels.push('Invite only');
    if (modes.join_key_configured) labels.push('Join key');
    if (modes.moderated) labels.push('Moderated');
    if (modes.no_outside_messages) labels.push('Members post');
    if (modes.topic_operators_only) labels.push('Operator topics');
    if (labels.length === 1 && !room.registered) labels.push('Open');
    return labels;
}

function _channelHubAdminActionLabel(action) {
    var labels = {
        register: 'registered the channel',
        unregister: 'unregistered the channel',
        topic: 'changed the topic',
        mode: 'changed channel policy',
        op: 'granted operator access',
        deop: 'removed operator access',
        voice: 'granted voice',
        devoice: 'removed voice',
        ban: 'added a channel ban',
        unban: 'removed a channel ban',
        kick: 'removed someone from the channel',
        invite: 'added an invitation',
        uninvite: 'removed an invitation',
        kline_added: 'added a hub ban',
        kline_removed: 'removed a hub ban',
        announce_failed: 'could not announce the hub',
        envelope_oversize: 'dropped an oversized outbound envelope',
        send_failed: 'could not deliver an outbound envelope'
    };
    return labels[String(action || '')] || 'changed hub state';
}

function _channelHubAdminEvidenceModel(entry) {
    entry = entry || {};
    var kind = String(entry.kind || '');
    var room = entry.room ? '#' + entry.room : '';
    var source = entry.source_nickname ||
        _channelHubAdminShortIdentity(entry.source_identity_hash);
    var target = entry.target_identity_hash
        ? _channelHubAdminShortIdentity(entry.target_identity_hash)
        : '';
    var title = 'Hub activity';
    var detail = room;
    if (kind === 'message') {
        title = source + ' posted in ' + room;
        detail = 'Message accepted and relayed';
    } else if (kind === 'action') {
        title = source + ' shared an action in ' + room;
        detail = 'Action accepted and relayed';
    } else if (kind === 'notice') {
        title = source + ' sent a notice in ' + room;
        detail = 'Notice accepted and relayed';
    } else if (kind === 'join') {
        title = source + ' joined ' + room;
        detail = entry.source_identity_hash || '';
    } else if (kind === 'part') {
        title = source + ' left ' + room;
        detail = entry.source_identity_hash || '';
    } else if (kind === 'moderation') {
        title = source + ' ' + _channelHubAdminActionLabel(entry.action);
        detail = [room, target].filter(Boolean).join(' \u00b7 ');
    } else if (kind === 'trust') {
        title = source + ' ' + _channelHubAdminActionLabel(entry.action);
        detail = target;
    } else if (kind === 'service') {
        title = _channelHubAdminActionLabel(entry.action);
        detail = entry.count ? _channelHubPlural(Number(entry.count), 'event') : 'Hub service';
    }
    return {
        title: title,
        detail: detail,
        excerpt: entry.excerpt || ''
    };
}

function _channelHubAdminBadge(text, tone) {
    var badge = _channelHubAdminNode('span', 'channel-host-admin-badge', text);
    if (tone) badge.dataset.tone = tone;
    return badge;
}

function _channelHubAdminHeader(root, titleText, detailText, refreshHandler) {
    var header = _channelHubAdminNode('div', 'channel-host-admin-panel-header');
    var copy = _channelHubAdminNode('div', 'channel-host-admin-panel-heading');
    copy.appendChild(_channelHubAdminNode('h3', '', titleText));
    if (detailText) copy.appendChild(_channelHubAdminNode('p', '', detailText));
    header.appendChild(copy);
    if (refreshHandler) {
        var refresh = _channelHubAdminNode('button', 'nr-btn nr-btn-secondary channel-host-admin-refresh', 'Refresh');
        refresh.type = 'button';
        refresh.setAttribute('aria-label', 'Refresh ' + titleText.toLowerCase());
        refresh.addEventListener('click', refreshHandler);
        header.appendChild(refresh);
    }
    root.appendChild(header);
    return header;
}

function _channelHubAdminNotice(tone, titleText, detailText) {
    var notice = _channelHubAdminNode('div', 'channel-host-admin-notice');
    notice.dataset.tone = tone || 'info';
    notice.appendChild(_channelHubAdminNode('strong', '', titleText));
    if (detailText) notice.appendChild(_channelHubAdminNode('span', '', detailText));
    return notice;
}

function _channelHubAdminEmpty(titleText, detailText) {
    var empty = _channelHubAdminNode('div', 'channel-host-admin-empty');
    empty.appendChild(_channelHubAdminNode('strong', '', titleText));
    empty.appendChild(_channelHubAdminNode('span', '', detailText));
    return empty;
}

function _channelHubAdminMetricGrid(items) {
    var grid = _channelHubAdminNode('div', 'channel-host-admin-metrics');
    (items || []).forEach(function(item) {
        var metric = _channelHubAdminNode('div', 'channel-host-admin-metric');
        metric.appendChild(_channelHubAdminNode('strong', '', item.value));
        metric.appendChild(_channelHubAdminNode('span', '', item.label));
        if (item.detail) metric.appendChild(_channelHubAdminNode('small', '', item.detail));
        grid.appendChild(metric);
    });
    return grid;
}

function _channelHubAdminIdentityRow(identityHash, labelText, trailingText) {
    var row = _channelHubAdminNode('div', 'channel-host-admin-identity');
    var copy = _channelHubAdminNode('div', 'channel-host-admin-identity-copy');
    if (labelText) copy.appendChild(_channelHubAdminNode('strong', '', labelText));
    var code = _channelHubAdminNode('code', '', identityHash || 'Unknown identity');
    code.title = identityHash || '';
    copy.appendChild(code);
    row.appendChild(copy);
    if (trailingText) row.appendChild(_channelHubAdminNode('span', 'channel-host-admin-identity-meta', trailingText));
    if (identityHash) {
        var button = _channelHubAdminNode('button', 'channel-host-admin-copy', 'Copy');
        button.type = 'button';
        button.setAttribute('aria-label', 'Copy identity ' + identityHash);
        button.addEventListener('click', function() {
            RS.copyText(identityHash).then(function(ok) {
                if (typeof showToast === 'function') {
                    showToast(ok ? 'Identity copied' : 'Could not copy', ok ? 'toast-green' : 'toast-orange', 1800);
                }
            });
        });
        row.appendChild(button);
    }
    return row;
}

function _channelHubRenderAdminOverview(root, admin, refreshHandler) {
    root.textContent = '';
    _channelHubAdminHeader(
        root,
        'Overview',
        (admin.running ? 'Live process snapshot' : 'Saved policy while the hub is stopped') +
            ' \u00b7 ' + _channelHubAdminGeneratedLabel(admin),
        refreshHandler
    );
    root.appendChild(_channelHubAdminNotice(
        admin.running ? 'online' : 'neutral',
        admin.running ? 'Live mesh community' : 'Hub stopped',
        admin.running
            ? 'People and recent context exist only while this process is running.'
            : 'Channel policy and access lists remain available. People and recent context do not.'
    ));

    var people = admin.running && Array.isArray(admin.people) ? admin.people : [];
    var rooms = Array.isArray(admin.rooms) ? admin.rooms : [];
    var sessions = people.reduce(function(total, person) {
        return total + (Number(person.session_count) || 0);
    }, 0);
    var registered = rooms.filter(function(room) { return !!room.registered; }).length;
    root.appendChild(_channelHubAdminMetricGrid([
        { value: people.length, label: 'People', detail: 'Unique identities' },
        {
            value: sessions,
            label: 'Live sessions',
            detail: _channelHubPlural(
                admin.running ? Number(admin.pending_sessions) || 0 : 0,
                'pending handshake'
            )
        },
        { value: rooms.length, label: 'Channels', detail: registered + ' registered' },
        { value: _channelHubAdminDuration(admin.uptime_secs), label: 'Uptime', detail: admin.running ? 'This run' : 'Not running' }
    ]));

    var stats = admin.stats || {};
    var forwarded = (Number(stats.messages_forwarded) || 0) +
        (Number(stats.notices_forwarded) || 0) +
        (Number(stats.actions_forwarded) || 0);
    var refused = (Number(stats.rate_limited) || 0) +
        (Number(stats.bad_packets) || 0) +
        (Number(stats.duplicates) || 0) +
        (Number(stats.resources_rejected) || 0) +
        (Number(stats.oversize) || 0);
    var activity = _channelHubAdminNode('section', 'channel-host-admin-section');
    activity.appendChild(_channelHubAdminNode('h4', '', 'This run'));
    activity.appendChild(_channelHubAdminMetricGrid([
        { value: forwarded, label: 'Room relays' },
        { value: (Number(stats.joins) || 0) + (Number(stats.parts) || 0), label: 'Membership changes' },
        { value: refused, label: 'Refused or dropped' },
        { value: Number(stats.resources_received) || 0, label: 'Large notices received' }
    ]));
    root.appendChild(activity);

    root.appendChild(_channelHubAdminNotice(
        'privacy',
        'Policy is durable. Conversation traffic is not.',
        'The hub stores registered channel settings and access lists, never transcripts or rosters.'
    ));
}

function _channelHubRenderAdminChannels(root, admin, refreshHandler) {
    root.textContent = '';
    var rooms = Array.isArray(admin.rooms) ? admin.rooms : [];
    _channelHubAdminHeader(
        root,
        'Channels',
        _channelHubPlural(rooms.length, 'channel') + ' \u00b7 ' +
            _channelHubAdminGeneratedLabel(admin),
        refreshHandler
    );
    if (!rooms.length) {
        root.appendChild(_channelHubAdminEmpty(
            'No channels yet',
            admin.running ? 'Creating and managing channels will be enabled in the next milestone.' : 'Start the hub before creating a channel.'
        ));
        return;
    }
    if (!admin.running) {
        root.appendChild(_channelHubAdminNotice(
            'neutral',
            'Showing saved policy',
            'Live membership counts return when the hub starts.'
        ));
    }
    var list = _channelHubAdminNode('div', 'channel-host-admin-list');
    rooms.forEach(function(room) {
        var row = _channelHubAdminNode('article', 'channel-host-admin-room');
        var heading = _channelHubAdminNode('div', 'channel-host-admin-room-heading');
        var nameCopy = _channelHubAdminNode('div', 'channel-host-admin-room-copy');
        nameCopy.appendChild(_channelHubAdminNode('strong', '', '#' + room.name));
        nameCopy.appendChild(_channelHubAdminNode('span', '', room.topic || 'No topic set'));
        heading.appendChild(nameCopy);
        heading.appendChild(_channelHubAdminBadge(
            room.registered ? 'Saved' : 'Live only',
            room.registered ? 'online' : 'neutral'
        ));
        row.appendChild(heading);
        var badges = _channelHubAdminNode('div', 'channel-host-admin-badges');
        _channelHubAdminModeLabels(room).forEach(function(label) {
            badges.appendChild(_channelHubAdminBadge(label, label === 'Private' || label === 'Invite only' ? 'warning' : 'neutral'));
        });
        row.appendChild(badges);
        var memberText = admin.running
            ? _channelHubPlural(Number(room.live_member_count) || 0, 'person', 'people') +
                ' \u00b7 ' + _channelHubPlural(Number(room.live_session_count) || 0, 'session')
            : 'Saved channel policy';
        var accessCount = (room.operators || []).length + (room.voiced || []).length +
            (room.bans || []).length + (room.invitations || []).length;
        row.appendChild(_channelHubAdminNode(
            'div',
            'channel-host-admin-room-meta',
            memberText + ' \u00b7 ' + _channelHubPlural(accessCount, 'access entry', 'access entries')
        ));
        if (room.save_pending) {
            row.appendChild(_channelHubAdminNotice('warning', 'Save pending', 'The durable registry is retrying this channel.'));
        }
        list.appendChild(row);
    });
    root.appendChild(list);
}

function _channelHubRenderAdminPeople(root, admin, refreshHandler) {
    root.textContent = '';
    var people = admin.running && Array.isArray(admin.people) ? admin.people : [];
    _channelHubAdminHeader(
        root,
        'People',
        _channelHubPlural(people.length, 'live identity', 'live identities') +
            ' \u00b7 ' + _channelHubAdminGeneratedLabel(admin),
        refreshHandler
    );
    if (!people.length) {
        root.appendChild(_channelHubAdminEmpty(
            admin.running ? 'No one is here yet' : 'People are live-only',
            admin.running ? 'Connected identities will appear here.' : 'Start the hub to see connected people and their current authority.'
        ));
        return;
    }
    var list = _channelHubAdminNode('div', 'channel-host-admin-list');
    people.forEach(function(person) {
        var row = _channelHubAdminNode('article', 'channel-host-admin-person');
        var name = person.nickname || _channelHubAdminShortIdentity(person.identity_hash);
        row.appendChild(_channelHubAdminIdentityRow(
            person.identity_hash,
            name,
            _channelHubPlural(Number(person.session_count) || 0, 'session')
        ));
        var badges = _channelHubAdminNode('div', 'channel-host-admin-badges');
        if (person.server_operator) badges.appendChild(_channelHubAdminBadge('Hub operator', 'accent'));
        if ((person.room_operator_in || []).length) {
            badges.appendChild(_channelHubAdminBadge(
                'Operator in ' + (person.room_operator_in || []).length,
                'neutral'
            ));
        }
        if ((person.voiced_in || []).length) {
            badges.appendChild(_channelHubAdminBadge('Voice in ' + (person.voiced_in || []).length, 'neutral'));
        }
        if ((Number(person.welcomed_session_count) || 0) < (Number(person.session_count) || 0)) {
            badges.appendChild(_channelHubAdminBadge('Handshake pending', 'warning'));
        }
        if (badges.children.length) row.appendChild(badges);
        var rooms = (person.rooms || []).map(function(room) { return '#' + room; });
        row.appendChild(_channelHubAdminNode(
            'p',
            'channel-host-admin-person-meta',
            (rooms.length ? rooms.join(', ') : 'Not in a channel') +
                ' \u00b7 connected ' + _channelHubAdminDuration(person.connected_for_secs)
        ));
        list.appendChild(row);
    });
    root.appendChild(list);
}

function _channelHubAdminIdentityGroup(root, titleText, identities, emptyText) {
    var group = _channelHubAdminNode('section', 'channel-host-admin-section');
    group.appendChild(_channelHubAdminNode('h4', '', titleText));
    if (!identities || !identities.length) {
        group.appendChild(_channelHubAdminNode('p', 'channel-host-admin-muted', emptyText));
    } else {
        var list = _channelHubAdminNode('div', 'channel-host-admin-identity-list');
        identities.forEach(function(identity) {
            list.appendChild(_channelHubAdminIdentityRow(identity));
        });
        group.appendChild(list);
    }
    root.appendChild(group);
}

function _channelHubRenderAdminAccess(root, admin, refreshHandler) {
    root.textContent = '';
    _channelHubAdminHeader(
        root,
        'Access',
        'Hub-wide and per-channel authority \u00b7 ' + _channelHubAdminGeneratedLabel(admin),
        refreshHandler
    );
    _channelHubAdminIdentityGroup(
        root,
        'Hub operators',
        admin.server_operators || [],
        'No hub operators are configured.'
    );
    _channelHubAdminIdentityGroup(
        root,
        'Hub bans',
        admin.hub_bans || [],
        'No identities are banned from this hub.'
    );

    var rooms = Array.isArray(admin.rooms) ? admin.rooms : [];
    var roomSection = _channelHubAdminNode('section', 'channel-host-admin-section');
    roomSection.appendChild(_channelHubAdminNode('h4', '', 'Channel access'));
    if (!rooms.length) {
        roomSection.appendChild(_channelHubAdminNode('p', 'channel-host-admin-muted', 'No channel access lists yet.'));
    }
    rooms.forEach(function(room) {
        var detail = _channelHubAdminNode('details', 'channel-host-admin-access-room');
        var accessCount = (room.operators || []).length + (room.voiced || []).length +
            (room.bans || []).length + (room.invitations || []).length;
        var summary = _channelHubAdminNode('summary', '', '#' + room.name);
        summary.appendChild(_channelHubAdminNode('span', '', _channelHubPlural(accessCount, 'entry', 'entries')));
        detail.appendChild(summary);
        var body = _channelHubAdminNode('div', 'channel-host-admin-access-room-body');
        [
            ['Operators', room.operators || []],
            ['Voiced identities', room.voiced || []],
            ['Channel bans', room.bans || []]
        ].forEach(function(group) {
            var block = _channelHubAdminNode('div', 'channel-host-admin-access-group');
            block.appendChild(_channelHubAdminNode('strong', '', group[0]));
            if (!group[1].length) {
                block.appendChild(_channelHubAdminNode('span', '', 'None'));
            } else {
                group[1].forEach(function(identity) {
                    block.appendChild(_channelHubAdminIdentityRow(identity));
                });
            }
            body.appendChild(block);
        });
        var inviteBlock = _channelHubAdminNode('div', 'channel-host-admin-access-group');
        inviteBlock.appendChild(_channelHubAdminNode('strong', '', 'Invitations'));
        if (!(room.invitations || []).length) {
            inviteBlock.appendChild(_channelHubAdminNode('span', '', 'None'));
        } else {
            (room.invitations || []).forEach(function(invitation) {
                var expiry = _channelHubAdminDate(invitation.expires_at_ms);
                inviteBlock.appendChild(_channelHubAdminIdentityRow(
                    invitation.identity_hash,
                    '',
                    expiry ? 'Expires ' + expiry.toLocaleString() : 'Expiry unavailable'
                ));
            });
        }
        body.appendChild(inviteBlock);
        detail.appendChild(body);
        roomSection.appendChild(detail);
    });
    root.appendChild(roomSection);
}

function _channelHubRenderAdminActivity(root, admin, refreshHandler) {
    root.textContent = '';
    var evidence = Array.isArray(admin.evidence) ? admin.evidence : [];
    var policy = admin.evidence_policy || {};
    _channelHubAdminHeader(
        root,
        'Activity',
        'Recent context for moderation decisions \u00b7 ' +
            _channelHubAdminGeneratedLabel(admin),
        refreshHandler
    );
    root.appendChild(_channelHubAdminNotice(
        'privacy',
        'Recent context, not a transcript',
        'Memory-only and incomplete: up to ' + _channelHubAdminDuration(policy.retention_secs) +
            ', ' + (Number(policy.max_events) || 0) + ' events, ' +
            _channelHubAdminBytes(policy.max_estimated_bytes) + ' total. ' +
            'Excerpts are display-sanitized and capped at ' +
            _channelHubAdminBytes(policy.max_excerpt_bytes) + '.'
    ));
    if (!admin.running) {
        root.appendChild(_channelHubAdminEmpty(
            'No activity while stopped',
            'Evidence is never persisted across a hub stop or restart.'
        ));
        return;
    }
    if (Number(admin.evidence_evicted) > 0) {
        root.appendChild(_channelHubAdminNotice(
            'neutral',
            _channelHubPlural(Number(admin.evidence_evicted), 'event') + ' no longer available',
            'Older, count-limited, or byte-limited context has been evicted.'
        ));
    }
    if (!evidence.length) {
        root.appendChild(_channelHubAdminEmpty(
            'No recent room activity',
            'Accepted room messages and moderation changes will appear here for a short time.'
        ));
        return;
    }
    var list = _channelHubAdminNode('div', 'channel-host-admin-timeline');
    evidence.forEach(function(entry) {
        var model = _channelHubAdminEvidenceModel(entry);
        var item = _channelHubAdminNode('article', 'channel-host-admin-event');
        item.dataset.sequence = String(entry.sequence || '');
        var marker = _channelHubAdminNode('i', 'channel-host-admin-event-marker');
        marker.dataset.kind = String(entry.kind || '');
        marker.setAttribute('aria-hidden', 'true');
        item.appendChild(marker);
        var body = _channelHubAdminNode('div', 'channel-host-admin-event-body');
        var heading = _channelHubAdminNode('div', 'channel-host-admin-event-heading');
        heading.appendChild(_channelHubAdminNode('strong', '', model.title));
        var observed = _channelHubAdminDate(entry.observed_at_ms);
        var time = _channelHubAdminNode(
            'time',
            '',
            observed ? observed.toLocaleTimeString([], {
                hour: 'numeric',
                minute: '2-digit'
            }) : 'Time unavailable'
        );
        if (observed) time.dateTime = observed.toISOString();
        heading.appendChild(time);
        body.appendChild(heading);
        if (model.detail) body.appendChild(_channelHubAdminNode('span', 'channel-host-admin-event-detail', model.detail));
        if (model.excerpt) body.appendChild(_channelHubAdminNode('p', 'channel-host-admin-event-excerpt', model.excerpt));
        item.appendChild(body);
        list.appendChild(item);
    });
    root.appendChild(list);
}

function _channelHubRenderAdminLimits(root, admin) {
    root.textContent = '';
    var limits = admin && admin.limits || {};
    var section = _channelHubAdminNode('section', 'channel-host-section channel-host-limits');
    section.appendChild(_channelHubAdminNode('h3', '', 'Operating limits'));
    section.appendChild(_channelHubAdminMetricGrid([
        { value: Number(limits.max_registered_rooms) || 0, label: 'Registered channels' },
        { value: Number(limits.max_rooms_per_session) || 0, label: 'Channels per session' },
        { value: _channelHubAdminBytes(limits.max_message_body_bytes), label: 'Message body' },
        { value: (Number(limits.rate_messages_per_minute) || 0) + '/min', label: 'Per-session rate' },
        { value: _channelHubAdminDuration(limits.invite_timeout_secs), label: 'Invitation lifetime' },
        { value: _channelHubAdminDuration(limits.rejoin_grace_secs), label: 'Reconnect grace' },
        { value: _channelHubAdminBytes(limits.max_resource_notice_bytes), label: 'Large room notice' },
        { value: _channelHubAdminBytes(limits.max_resource_bytes), label: 'Resource ceiling' }
    ]));
    section.appendChild(_channelHubAdminNode(
        'p',
        'channel-host-admin-muted',
        'These safety limits are enforced by the hub and are not editable in this release.'
    ));
    root.appendChild(section);
}

function _channelHubRenderAdminLoading(root, titleText) {
    root.textContent = '';
    _channelHubAdminHeader(root, titleText, 'Loading local owner state');
    var loading = _channelHubAdminNode('div', 'channel-host-admin-loading');
    loading.appendChild(_channelHubAdminNode('span', 'loading-spinner'));
    loading.appendChild(_channelHubAdminNode('span', '', 'Loading\u2026'));
    root.appendChild(loading);
}

function _channelHubRenderAdminError(root, titleText, error, retryHandler) {
    root.textContent = '';
    _channelHubAdminHeader(root, titleText, 'Owner state is temporarily unavailable');
    var failure = _channelHubAdminEmpty(
        'Could not load ' + titleText.toLowerCase(),
        error && error.message ? error.message : 'Try again when the local runtime is ready.'
    );
    var retry = _channelHubAdminNode('button', 'nr-btn nr-btn-secondary', 'Try again');
    retry.type = 'button';
    retry.addEventListener('click', retryHandler);
    failure.appendChild(retry);
    root.appendChild(failure);
}

function channelHubOpenManager(initialOverview) {
    if (typeof _rsBuildSheet !== 'function') return;
    if (_channelHubManagerDismiss) {
        var previousDismiss = _channelHubManagerDismiss;
        _channelHubManagerDismiss = null;
        previousDismiss();
    }
    var sequence = ++_channelHubManagerSequence;
    var overview = initialOverview || channelHubOverview;
    if (!overview) {
        channelHubLoad(true).then(function(nextOverview) {
            if (_channelHubManagerSequence === sequence && nextOverview) {
                channelHubOpenManager(nextOverview);
            }
        }).catch(function(error) {
            if (typeof showToast === 'function') showToast((error && error.message) || 'Could not load your hub', 'toast-red', 3200);
        });
        return;
    }

    var built = _rsBuildSheet({ title: 'Hub administration' }, function() {
        if (_channelHubManagerSequence !== sequence) return;
        _channelHubStatusRenderer = null;
        _channelHubManagerDismiss = null;
        _channelHubManagerSequence += 1;
    });
    _channelHubManagerDismiss = built.dismiss;
    built.sheet.classList.add('channel-host-admin-sheet');
    built.body.classList.add('channel-host-admin-body');
    built.footer.classList.add('channel-host-admin-footer');

    var hero = document.createElement('div');
    hero.className = 'channel-host-hero';
    var heroIcon = document.createElement('span');
    heroIcon.className = 'channel-host-hero-icon';
    heroIcon.innerHTML = _channelHubIcon('host');
    var heroCopy = document.createElement('div');
    heroCopy.className = 'channel-host-hero-copy';
    var heroName = document.createElement('strong');
    var heroStatus = document.createElement('span');
    heroStatus.className = 'channel-host-status';
    var statusDot = document.createElement('i');
    statusDot.setAttribute('aria-hidden', 'true');
    var statusLabel = document.createElement('span');
    heroStatus.appendChild(statusDot);
    heroStatus.appendChild(statusLabel);
    heroCopy.appendChild(heroName);
    heroCopy.appendChild(heroStatus);
    var stateButton = document.createElement('button');
    stateButton.type = 'button';
    stateButton.className = 'nr-btn channel-host-state-btn';
    hero.appendChild(heroIcon);
    hero.appendChild(heroCopy);
    hero.appendChild(stateButton);
    built.body.appendChild(hero);

    var address = document.createElement('div');
    address.className = 'channel-host-address';
    var addressCopy = document.createElement('div');
    addressCopy.className = 'channel-host-address-copy';
    var addressLabel = document.createElement('span');
    addressLabel.textContent = 'Hub address';
    var addressValue = document.createElement('code');
    addressCopy.appendChild(addressLabel);
    addressCopy.appendChild(addressValue);
    var copyAddress = document.createElement('button');
    copyAddress.type = 'button';
    copyAddress.className = 'channel-host-copy-btn';
    copyAddress.title = 'Copy hub address';
    copyAddress.setAttribute('aria-label', 'Copy hub address');
    copyAddress.innerHTML = '<svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>';
    copyAddress.addEventListener('click', function() {
        var value = addressValue.textContent;
        if (!value) return;
        RS.copyText(value).then(function(ok) {
            if (typeof showToast === 'function') showToast(ok ? 'Hub address copied' : 'Could not copy', ok ? 'toast-green' : 'toast-orange', 1800);
        });
    });
    address.appendChild(addressCopy);
    address.appendChild(copyAddress);
    built.body.appendChild(address);

    var registryWarning = document.createElement('div');
    registryWarning.className = 'channel-host-registry-warning';
    registryWarning.setAttribute('role', 'status');
    registryWarning.textContent = 'Some channel changes are still waiting to be saved.';
    registryWarning.hidden = true;
    built.body.appendChild(registryWarning);

    var tabDefinitions = [
        { id: 'overview', label: 'Overview' },
        { id: 'channels', label: 'Channels' },
        { id: 'people', label: 'People' },
        { id: 'access', label: 'Access' },
        { id: 'activity', label: 'Activity' },
        { id: 'settings', label: 'Settings' }
    ];
    var tabs = document.createElement('div');
    tabs.className = 'channel-host-admin-tabs';
    tabs.setAttribute('role', 'tablist');
    tabs.setAttribute('aria-label', 'Hub administration');
    var panelHost = document.createElement('div');
    panelHost.className = 'channel-host-admin-panels';
    var tabButtons = {};
    var panels = {};
    tabDefinitions.forEach(function(definition, index) {
        var tab = document.createElement('button');
        tab.type = 'button';
        tab.className = 'channel-host-admin-tab';
        tab.id = 'channel-host-admin-tab-' + sequence + '-' + definition.id;
        tab.textContent = definition.label;
        tab.setAttribute('role', 'tab');
        tab.setAttribute('aria-controls', 'channel-host-admin-panel-' + sequence + '-' + definition.id);
        tab.setAttribute('aria-selected', 'false');
        tab.tabIndex = -1;
        tab.addEventListener('click', function() {
            setActiveTab(definition.id, false);
        });
        tab.addEventListener('keydown', function(event) {
            var nextIndex = index;
            if (event.key === 'ArrowRight') nextIndex = (index + 1) % tabDefinitions.length;
            else if (event.key === 'ArrowLeft') nextIndex = (index + tabDefinitions.length - 1) % tabDefinitions.length;
            else if (event.key === 'Home') nextIndex = 0;
            else if (event.key === 'End') nextIndex = tabDefinitions.length - 1;
            else return;
            event.preventDefault();
            setActiveTab(tabDefinitions[nextIndex].id, true);
        });
        tabs.appendChild(tab);
        tabButtons[definition.id] = tab;

        var panel = document.createElement('section');
        panel.className = 'channel-host-admin-panel';
        panel.id = 'channel-host-admin-panel-' + sequence + '-' + definition.id;
        panel.setAttribute('role', 'tabpanel');
        panel.setAttribute('aria-labelledby', tab.id);
        panel.hidden = true;
        panelHost.appendChild(panel);
        panels[definition.id] = panel;
    });
    built.body.appendChild(tabs);
    built.body.appendChild(panelHost);

    var settings = overview.settings || {};
    _channelHubAdminHeader(
        panels.settings,
        'Settings',
        'Hub identity, discovery, and resource policy'
    );
    var profile = document.createElement('section');
    profile.className = 'channel-host-section';
    var profileTitle = document.createElement('h3');
    profileTitle.textContent = 'Profile';
    profile.appendChild(profileTitle);

    var nameInput = document.createElement('input');
    nameInput.type = 'text';
    nameInput.className = 'nr-input-sm';
    nameInput.maxLength = 64;
    nameInput.value = settings.hub_name || 'Ratspeak Hub';
    profile.appendChild(_channelHubField('Hub name', nameInput, 'Shown when people discover your hub'));

    var greetingInput = document.createElement('textarea');
    greetingInput.className = 'nr-input-sm channel-host-greeting';
    greetingInput.maxLength = 512;
    greetingInput.rows = 3;
    greetingInput.placeholder = 'Welcome people and tell them where to begin';
    greetingInput.value = settings.greeting || '';
    var greetingField = _channelHubField('Welcome message', greetingInput, 'Shown once when someone connects');
    var greetingCount = greetingField.querySelector('.channel-host-field-hint');
    profile.appendChild(greetingField);
    panels.settings.appendChild(profile);

    var discovery = document.createElement('section');
    discovery.className = 'channel-host-section';
    var discoveryTitle = document.createElement('h3');
    discoveryTitle.textContent = 'Discovery';
    discovery.appendChild(discoveryTitle);
    var discoveryRow = document.createElement('div');
    discoveryRow.className = 'channel-host-discovery-row';
    var discoveryCopy = document.createElement('div');
    discoveryCopy.className = 'channel-host-discovery-copy';
    var discoveryLabel = document.createElement('label');
    discoveryLabel.textContent = 'Announce this hub';
    var discoveryHint = document.createElement('span');
    discoveryHint.textContent = 'Help nearby people find it without an address';
    discoveryCopy.appendChild(discoveryLabel);
    discoveryCopy.appendChild(discoveryHint);
    var announceInput = document.createElement('select');
    announceInput.className = 'nr-select channel-host-announce-select';
    [
        [0, 'When started'],
        [300, 'Every 5 min'],
        [900, 'Every 15 min'],
        [1800, 'Every 30 min'],
        [3600, 'Every hour'],
        [21600, 'Every 6 hours'],
        [86400, 'Every day']
    ].forEach(function(optionValue) {
        var option = document.createElement('option');
        option.value = String(optionValue[0]);
        option.textContent = optionValue[1];
        announceInput.appendChild(option);
    });
    announceInput.value = String(Number(settings.announce_interval_secs) || 0);
    discoveryLabel.htmlFor = 'channel-host-announce-' + sequence;
    announceInput.id = discoveryLabel.htmlFor;
    discoveryRow.appendChild(discoveryCopy);
    discoveryRow.appendChild(announceInput);
    discovery.appendChild(discoveryRow);
    panels.settings.appendChild(discovery);

    var advanced = document.createElement('details');
    advanced.className = 'channel-host-advanced';
    var advancedSummary = document.createElement('summary');
    advancedSummary.textContent = 'Advanced';
    advanced.appendChild(advancedSummary);
    var advancedBody = document.createElement('div');
    advancedBody.className = 'channel-host-advanced-body';
    var sendToggle = _channelHubToggle(
        'Large welcome messages',
        'Deliver longer welcome text when it cannot fit in one packet',
        settings.resource_send_enabled
    );
    var acceptToggle = _channelHubToggle(
        'Large room notices',
        'Accept larger notices from people already allowed to post',
        settings.resource_accept_enabled
    );
    advancedBody.appendChild(sendToggle.row);
    advancedBody.appendChild(acceptToggle.row);
    advanced.appendChild(advancedBody);
    panels.settings.appendChild(advanced);

    var limitsHost = document.createElement('div');
    limitsHost.className = 'channel-host-admin-settings-limits';
    panels.settings.appendChild(limitsHost);

    var impact = document.createElement('p');
    impact.className = 'channel-host-impact';
    impact.hidden = true;
    impact.textContent = 'Saving will briefly restart the hub so these changes can take effect.';
    panels.settings.appendChild(impact);

    var error = document.createElement('div');
    error.className = 'channel-sheet-error channel-host-error';
    error.setAttribute('aria-live', 'polite');
    panels.settings.appendChild(error);

    var close = document.createElement('button');
    close.type = 'button';
    close.className = 'nr-btn nr-btn-secondary';
    close.textContent = 'Close';
    close.addEventListener('click', function() { built.dismiss(); });
    var save = document.createElement('button');
    save.type = 'button';
    save.className = 'nr-btn nr-btn-primary';
    save.textContent = 'Save changes';
    save.disabled = true;
    built.footer.appendChild(close);
    built.footer.appendChild(save);

    var controls = [nameInput, greetingInput, announceInput, sendToggle.input, acceptToggle.input];
    var busy = false;
    var activeTab = 'overview';
    var adminSnapshot = null;
    var adminRequest = 0;
    var adminPanelTitles = {
        overview: 'Overview',
        channels: 'Channels',
        people: 'People',
        access: 'Access',
        activity: 'Activity'
    };

    function setActiveTab(tabId, focusTab) {
        if (!panels[tabId]) return;
        activeTab = tabId;
        tabDefinitions.forEach(function(definition) {
            var selected = definition.id === tabId;
            tabButtons[definition.id].classList.toggle('active', selected);
            tabButtons[definition.id].setAttribute('aria-selected', selected ? 'true' : 'false');
            tabButtons[definition.id].tabIndex = selected ? 0 : -1;
            panels[definition.id].hidden = !selected;
        });
        save.hidden = tabId !== 'settings';
        built.footer.dataset.activeTab = tabId;
        if (focusTab) tabButtons[tabId].focus();
        renderDirty();
    }

    function renderAdminLoading() {
        Object.keys(adminPanelTitles).forEach(function(tabId) {
            _channelHubRenderAdminLoading(panels[tabId], adminPanelTitles[tabId]);
        });
        _channelHubRenderAdminLoading(limitsHost, 'Operating limits');
    }

    function renderAdmin(nextAdmin) {
        var liveStatus = overview.status || {};
        registryWarning.hidden = !(liveStatus.registry_degraded || nextAdmin.registry_degraded);
        _channelHubRenderAdminOverview(panels.overview, nextAdmin, refreshAdmin);
        _channelHubRenderAdminChannels(panels.channels, nextAdmin, refreshAdmin);
        _channelHubRenderAdminPeople(panels.people, nextAdmin, refreshAdmin);
        _channelHubRenderAdminAccess(panels.access, nextAdmin, refreshAdmin);
        _channelHubRenderAdminActivity(panels.activity, nextAdmin, refreshAdmin);
        _channelHubRenderAdminLimits(limitsHost, nextAdmin);
    }

    function renderAdminError(loadError) {
        Object.keys(adminPanelTitles).forEach(function(tabId) {
            _channelHubRenderAdminError(
                panels[tabId],
                adminPanelTitles[tabId],
                loadError,
                refreshAdmin
            );
        });
        _channelHubRenderAdminError(
            limitsHost,
            'Operating limits',
            loadError,
            refreshAdmin
        );
    }

    function loadAdmin() {
        var request = ++adminRequest;
        if (!adminSnapshot) renderAdminLoading();
        return RS.invoke('api_channel_hub_admin').then(function(nextAdmin) {
            if (_channelHubManagerSequence !== sequence || request !== adminRequest) return null;
            if (!nextAdmin || Number(nextAdmin.model_version) !== 1) {
                throw new Error('This hub admin snapshot uses an unsupported model version.');
            }
            if (!nextAdmin.evidence_policy || nextAdmin.evidence_policy.persistent !== false) {
                throw new Error('This hub admin snapshot does not satisfy the memory-only evidence contract.');
            }
            adminSnapshot = nextAdmin;
            renderAdmin(adminSnapshot);
            return adminSnapshot;
        }).catch(function(loadError) {
            if (_channelHubManagerSequence !== sequence || request !== adminRequest) return null;
            adminSnapshot = null;
            renderAdminError(loadError);
            return null;
        });
    }

    function refreshAdmin() {
        return loadAdmin();
    }

    function currentArgs() {
        return _channelHubConfigArgs(
            nameInput,
            greetingInput,
            announceInput,
            sendToggle.input,
            acceptToggle.input
        );
    }

    function renderDirty() {
        var dirty = !_channelHubSettingsEqual(overview.settings, currentArgs());
        save.disabled = busy || !dirty || !nameInput.value.trim();
        save.hidden = activeTab !== 'settings';
        impact.hidden = !(dirty && overview.status && overview.status.running);
        greetingCount.textContent = (greetingInput.value.length || 0) + '/512 · Shown once when someone connects';
    }

    function setBusy(value, label) {
        busy = value;
        controls.forEach(function(control) { control.disabled = value; });
        stateButton.disabled = value;
        close.disabled = value;
        save.disabled = value || _channelHubSettingsEqual(overview.settings, currentArgs());
        if (label) stateButton.textContent = label;
    }

    function renderStatus(nextOverview) {
        if (!nextOverview || _channelHubManagerSequence !== sequence) return;
        overview = nextOverview;
        var model = _channelHubStatusModel(overview);
        var liveSettings = overview.settings || {};
        var status = overview.status || {};
        heroName.textContent = liveSettings.hub_name || 'Ratspeak Hub';
        statusLabel.textContent = model.label;
        heroStatus.dataset.tone = model.tone;
        stateButton.dataset.action = model.action;
        stateButton.textContent = model.actionLabel;
        stateButton.className = model.action === 'start'
            ? 'nr-btn nr-btn-primary channel-host-state-btn'
            : 'nr-btn nr-btn-secondary channel-host-state-btn';
        stateButton.disabled = busy;
        var destination = status.destination_hash || overview.destination_hash || '';
        addressValue.textContent = destination;
        address.classList.toggle('is-empty', !destination);
        addressLabel.textContent = destination ? 'Hub address' : 'Hub address appears after the first start';
        copyAddress.hidden = !destination;
        copyAddress.disabled = !destination;
        registryWarning.hidden = !(status.registry_degraded ||
            (adminSnapshot && adminSnapshot.registry_degraded));
        renderDirty();
    }

    function applyResult(nextOverview, toastText) {
        if (_channelHubManagerSequence !== sequence) return null;
        overview = _channelHubApplyOverview(nextOverview);
        error.textContent = '';
        renderStatus(overview);
        if (toastText && typeof showToast === 'function') showToast(toastText, 'toast-green', 2200);
        return overview;
    }

    function saveConfig() {
        var args = currentArgs();
        if (!args.hub_name) return Promise.reject(new Error('Choose a name for your hub.'));
        return RS.invoke('channel_hub_set_config', { args: args }).then(function(nextOverview) {
            var updated = applyResult(nextOverview);
            if (!updated) return null;
            nameInput.value = updated.settings.hub_name || '';
            greetingInput.value = updated.settings.greeting || '';
            announceInput.value = String(Number(updated.settings.announce_interval_secs) || 0);
            sendToggle.input.checked = !!updated.settings.resource_send_enabled;
            acceptToggle.input.checked = !!updated.settings.resource_accept_enabled;
            renderDirty();
            return updated;
        });
    }

    controls.forEach(function(control) {
        control.addEventListener('input', renderDirty);
        control.addEventListener('change', renderDirty);
    });

    save.addEventListener('click', function() {
        setBusy(true);
        error.textContent = '';
        saveConfig().then(function(updated) {
            if (!updated || _channelHubManagerSequence !== sequence) return;
            if (typeof showToast === 'function') showToast('Hub settings saved', 'toast-green', 2200);
            refreshAdmin();
        }).catch(function(saveError) {
            if (_channelHubManagerSequence !== sequence) return;
            error.textContent = (saveError && saveError.message) || 'Could not save hub settings.';
        }).then(function() {
            if (_channelHubManagerSequence !== sequence) return;
            setBusy(false);
            renderStatus(overview);
        });
    });

    stateButton.addEventListener('click', function() {
        var action = stateButton.dataset.action;
        setBusy(true, action === 'start' ? 'Starting…' : 'Stopping…');
        error.textContent = '';
        var request = action === 'start'
            ? saveConfig().then(function(updated) {
                if (!updated || _channelHubManagerSequence !== sequence) return null;
                return RS.invoke('channel_hub_start');
            })
            : RS.invoke('channel_hub_stop');
        request.then(function(nextOverview) {
            if (!nextOverview || _channelHubManagerSequence !== sequence) return;
            applyResult(nextOverview, action === 'start' ? 'Your hub is ready' : 'Hub stopped');
            refreshAdmin();
        }).catch(function(actionError) {
            if (_channelHubManagerSequence !== sequence) return;
            error.textContent = (actionError && actionError.message) ||
                (action === 'start' ? 'Could not start your hub.' : 'Could not stop your hub.');
            return channelHubLoad(true).then(function(nextOverview) {
                if (_channelHubManagerSequence === sequence && nextOverview) {
                    overview = nextOverview;
                }
            }).catch(function() {});
        }).then(function() {
            if (_channelHubManagerSequence !== sequence) return;
            setBusy(false);
            renderStatus(overview);
        });
    });

    _channelHubStatusRenderer = renderStatus;
    renderStatus(overview);
    setActiveTab('overview', false);
    loadAdmin();
    _channelsPresentSheet(built, tabButtons.overview);
}

RS.listen('channel_hub_snapshot', function(status) {
    if (!channelHubOverview) return;
    channelHubOverview.status = status || {};
    if (status && status.destination_hash) {
        channelHubOverview.created = true;
        channelHubOverview.destination_hash = status.destination_hash;
    }
    _channelHubOverviewLoadedAt = Date.now();
    if (_channelHubStatusRenderer) _channelHubStatusRenderer(channelHubOverview);
    channelHubRenderHome(channelHubOverview);
});

RS.listen('lxmf_identity', function() {
    _channelHubIdentityGeneration += 1;
    _channelHubOverviewPromise = null;
    _channelHubManagerSequence += 1;
    _channelHubStatusRenderer = null;
    if (_channelHubManagerDismiss) {
        var dismissManager = _channelHubManagerDismiss;
        _channelHubManagerDismiss = null;
        dismissManager();
    }
    channelHubOverview = null;
    _channelHubOverviewLoadedAt = 0;
    channelHubRenderHome(null);
});

function channelHubOpenOwnHub() {
    var overview = channelHubOverview;
    var status = overview && overview.status || {};
    var destination = channelHubOwnDestinationHash();
    if (!overview || !status.running || !destination) {
        channelHubOpenManager(overview);
        return;
    }

    var currentDestination = _channelHubCurrentDestination();
    var current = currentDestination === destination;
    if (current && typeof _channelsIsConnecting === 'function' && _channelsIsConnecting()) return;
    if (current && typeof _channelsIsConnected === 'function' && _channelsIsConnected()) {
        if (typeof renderChannels === 'function') renderChannels();
        return;
    }

    var proceed = Promise.resolve(true);
    if (currentDestination && currentDestination !== destination &&
            typeof channelsSnapshot !== 'undefined' &&
            channelsSnapshot.phase !== 'offline' && channelsSnapshot.phase !== 'unavailable') {
        var currentName = typeof _channelsHubName === 'function'
            ? _channelsHubName(channelsSnapshot.hub)
            : 'your current hub';
        proceed = rsConfirm({
            title: 'Switch hubs?',
            message: 'This will leave ' + currentName + ' and connect to your hub.',
            confirmText: 'Switch hubs'
        });
    }

    proceed.then(function(confirmed) {
        if (!confirmed) return;
        if (typeof channelsConnectToHub !== 'function') {
            channelsOpenConnectSheet({
                destination_hash: destination,
                announced_name: overview.settings && overview.settings.hub_name
            });
            return;
        }
        _channelHubHomeBusy = true;
        channelHubRenderHome(overview);
        return channelsConnectToHub({
            destination_hash: destination,
            announced_name: overview.settings && overview.settings.hub_name,
            nickname: typeof _channelsDefaultNickname === 'function' ? _channelsDefaultNickname() : ''
        }).catch(function(error) {
            if (typeof showToast === 'function') {
                showToast((error && error.message) || 'Could not open your hub', 'toast-red', 3200);
            }
        }).then(function() {
            _channelHubHomeBusy = false;
            channelHubRenderHome(overview);
        });
    });
}

function _channelHubBindHome() {
    var open = document.getElementById('channel-owned-hub-open');
    var manage = document.getElementById('channel-owned-hub-manage');
    if (open && open.dataset.bound !== 'true') {
        open.dataset.bound = 'true';
        open.addEventListener('click', channelHubOpenOwnHub);
    }
    if (manage && manage.dataset.bound !== 'true') {
        manage.dataset.bound = 'true';
        manage.addEventListener('click', function() { channelHubOpenManager(channelHubOverview); });
    }
}

_channelHubBindHome();
