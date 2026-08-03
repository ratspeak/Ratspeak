// Desktop RRC hub hosting. Client-side Channels traffic remains in channels.js;
// this file owns only the operator surface and its stable IPC read model.

var channelHubOverview = null;
var _channelHubOverviewLoadedAt = 0;
var _channelHubOverviewPromise = null;
var _channelHubStatusRenderer = null;
var _channelHubManagerSequence = 0;
var _channelHubManagerDismiss = null;
var _channelHubAdminChildDismisses = [];
var _channelHubIdentityGeneration = 0;
var _channelHubHomeBusy = false;

function _channelHubPlural(count, singular, plural) {
    return count + ' ' + (count === 1 ? singular : (plural || singular + 's'));
}

function _channelHubHostingEnabled(overview) {
    // The Settings preference is the current UI authority. An overview request
    // that began before the user toggled Off must not resurrect hosting tools.
    if (typeof window.ratspeakChannelHostingEnabled === 'function') {
        return window.ratspeakChannelHostingEnabled();
    }
    // settings.js loads after this module. Until it establishes the explicit
    // preference, default closed rather than inheriting legacy overview state.
    return false;
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

function channelHubOwnDestinationHash() {
    if (!channelHubOverview) return '';
    var status = channelHubOverview.status || {};
    return String(status.destination_hash || channelHubOverview.destination_hash || '').toLowerCase();
}

function _channelHubHomeVisible(overview) {
    // Enabling the Settings capability must reveal the first-run entry point,
    // not only hubs that have already been configured. The card itself opens
    // setup when no hub exists yet and becomes the live hub card afterward.
    return !!(overview && overview.supported && _channelHubHostingEnabled(overview));
}

function _channelHubCurrentDestination() {
    if (typeof channelsSnapshot === 'undefined' || !channelsSnapshot.hub) return '';
    return String(channelsSnapshot.hub.destination_hash || '').toLowerCase();
}

function channelHubRenderHome(overview) {
    overview = overview || channelHubOverview;
    var section = document.getElementById('channel-owned-hub');
    if (!section) return;
    var visible = _channelHubHomeVisible(overview);
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
        statusText = connected ? 'Connected · ' + counts : (connecting ? 'Connecting…' : counts);
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
    if (kind === 'link') {
        return '<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="M10.5 13.5a4.5 4.5 0 0 0 6.4.1l2.1-2.1a4.5 4.5 0 0 0-6.4-6.4l-1.2 1.2"/><path d="M13.5 10.5a4.5 4.5 0 0 0-6.4-.1L5 12.5a4.5 4.5 0 0 0 6.4 6.4l1.2-1.2"/></svg>';
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
    function present(overview) {
        overview = overview || { supported: false };
        var built = _rsBuildSheet({ title: 'Manage Hub' }, function() {});
        built.sheet.classList.add('channel-hub-launch-sheet');

        var intro = document.createElement('p');
        intro.className = 'channel-sheet-copy';
        intro.textContent = 'Choose how you want to connect.';
        built.body.appendChild(intro);

        var join = _channelHubChoice(
            'join',
            'Join a hub',
            'Choose a nearby or saved hub, or enter an address'
        );
        join.addEventListener('click', function() {
            built.dismiss();
            setTimeout(function() {
                if (typeof channelsOpenHubSwitcher === 'function') {
                    channelsOpenHubSwitcher();
                } else {
                    channelsOpenConnectSheet();
                }
            }, 220);
        });
        built.body.appendChild(join);

        var shared = _channelHubChoice(
            'link',
            'Use a link or QR',
            'Preview a shared hub or channel before connecting'
        );
        shared.addEventListener('click', function() {
            built.dismiss();
            setTimeout(function() { channelsOpenSharedChannel(); }, 220);
        });
        built.body.appendChild(shared);

        if (overview.supported && _channelHubHostingEnabled(overview)) {
            var model = _channelHubStatusModel(overview);
            var host = _channelHubChoice(
                'host',
                overview.created || (overview.settings && overview.settings.enabled) ? 'Manage your hub' : 'Host a hub',
                model.detail,
                model.label
            );
            host.addEventListener('click', function() {
                built.dismiss();
                setTimeout(function() { channelHubOpenManager(overview); }, 220);
            });
            built.body.appendChild(host);
        }

        var cancel = document.createElement('button');
        cancel.type = 'button';
        cancel.className = 'nr-btn nr-btn-secondary';
        cancel.textContent = 'Cancel';
        cancel.addEventListener('click', function() { built.dismiss(); });
        built.footer.appendChild(cancel);
        _channelsPresentSheet(built, join);
    }

    channelHubLoad(true).then(function(overview) {
        present(overview);
    }).catch(function() {
        present({ supported: false });
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

function _channelHubConfigArgs(
    nameInput,
    greetingInput,
    announceInput,
    recentActivityInput
) {
    return {
        hub_name: nameInput.value.trim(),
        greeting: greetingInput.value.trim(),
        announce_interval_secs: Number(announceInput.value) || 0,
        recent_activity_retention_secs: Number(recentActivityInput.value) || 0
    };
}

function _channelHubSettingsEqual(settings, args) {
    settings = settings || {};
    return String(settings.hub_name || '') === args.hub_name &&
        String(settings.greeting || '') === args.greeting &&
        Number(settings.announce_interval_secs || 0) === args.announce_interval_secs &&
        Number(settings.recent_activity_retention_secs || 0) ===
            args.recent_activity_retention_secs;
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

function _channelHubAdminButton(labelText, handler, options) {
    options = options || {};
    var classes = options.primary
        ? 'nr-btn nr-btn-primary channel-host-admin-action'
        : 'nr-btn nr-btn-secondary channel-host-admin-action';
    if (options.danger) classes = 'nr-btn nr-btn-danger channel-host-admin-action';
    var button = _channelHubAdminNode('button', classes, labelText);
    button.type = 'button';
    button.disabled = !!options.disabled;
    if (options.title) button.title = options.title;
    if (handler) button.addEventListener('click', handler);
    return button;
}

function _channelHubAdminHeader(root, titleText, detailText, refreshHandler, actionItems) {
    var header = _channelHubAdminNode('div', 'channel-host-admin-panel-header');
    var copy = _channelHubAdminNode('div', 'channel-host-admin-panel-heading');
    copy.appendChild(_channelHubAdminNode('h3', '', titleText));
    if (detailText) copy.appendChild(_channelHubAdminNode('p', '', detailText));
    header.appendChild(copy);
    if (refreshHandler || (actionItems && actionItems.length)) {
        var actions = _channelHubAdminNode('div', 'channel-host-admin-header-actions');
        (actionItems || []).forEach(function(item) {
            actions.appendChild(_channelHubAdminButton(
                item.label,
                item.handler,
                item
            ));
        });
        if (refreshHandler) {
        var refresh = _channelHubAdminNode('button', 'nr-btn nr-btn-secondary channel-host-admin-refresh', 'Refresh');
        refresh.type = 'button';
        refresh.setAttribute('aria-label', 'Refresh ' + titleText.toLowerCase());
        refresh.addEventListener('click', refreshHandler);
            actions.appendChild(refresh);
        }
        header.appendChild(actions);
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

function _channelHubAdminIdentityRow(identityHash, labelText, trailingText, actionItems) {
    var row = _channelHubAdminNode('div', 'channel-host-admin-identity');
    var copy = _channelHubAdminNode('div', 'channel-host-admin-identity-copy');
    if (labelText) copy.appendChild(_channelHubAdminNode('strong', '', labelText));
    var code = _channelHubAdminNode('code', '', identityHash || 'Unknown identity');
    code.title = identityHash || '';
    copy.appendChild(code);
    row.appendChild(copy);
    if (trailingText) row.appendChild(_channelHubAdminNode('span', 'channel-host-admin-identity-meta', trailingText));
    if (identityHash || (actionItems && actionItems.length)) {
        var actions = _channelHubAdminNode('div', 'channel-host-admin-inline-actions');
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
            actions.appendChild(button);
        }
        (actionItems || []).forEach(function(item) {
            actions.appendChild(_channelHubAdminButton(item.label, item.handler, item));
        });
        row.appendChild(actions);
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
        { value: _channelHubAdminDuration(admin.uptime_secs), label: 'Uptime', detail: admin.running ? '' : 'Not running' }
    ]));
}

function _channelHubRenderAdminChannels(root, admin, refreshHandler, actions) {
    root.textContent = '';
    var rooms = Array.isArray(admin.rooms) ? admin.rooms : [];
    var mutationDisabled = !admin.running || !actions || !!actions.disabled;
    _channelHubAdminHeader(
        root,
        'Channels',
        _channelHubPlural(rooms.length, 'channel') + ' \u00b7 ' +
            _channelHubAdminGeneratedLabel(admin),
        refreshHandler,
        actions ? [{
            label: 'Create channel',
            primary: true,
            disabled: mutationDisabled,
            title: mutationDisabled && !admin.running
                ? 'Start the hub before creating a channel'
                : '',
            handler: actions.createChannel
        }] : null
    );
    if (!rooms.length) {
        root.appendChild(_channelHubAdminEmpty(
            'No channels yet',
            admin.running
                ? 'Create a registered channel to define durable policy and access.'
                : 'Start the hub before creating a channel.'
        ));
        return;
    }
    if (!admin.running) {
        root.appendChild(_channelHubAdminNotice(
            'neutral',
            'Showing saved policy',
            'Live membership counts return when the hub starts. Start the hub to change channel policy.'
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
        var headingActions = _channelHubAdminNode('div', 'channel-host-admin-room-actions');
        headingActions.appendChild(_channelHubAdminBadge(
            room.registered ? 'Saved' : 'Live only',
            room.registered ? 'online' : 'neutral'
        ));
        if (actions) {
            headingActions.appendChild(_channelHubAdminButton(
                room.registered ? 'Manage' : 'Register',
                function() { actions.editChannel(room); },
                {
                    disabled: mutationDisabled,
                    title: mutationDisabled && !admin.running
                        ? 'Start the hub before changing this channel'
                        : ''
                }
            ));
        }
        heading.appendChild(headingActions);
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

function _channelHubRenderAdminPeople(root, admin, refreshHandler, actions) {
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
            _channelHubPlural(Number(person.session_count) || 0, 'session'),
            actions ? [{
                label: 'Manage access',
                disabled: !admin.running || !!actions.disabled,
                handler: function() { actions.managePerson(person); }
            }] : null
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

function _channelHubAdminIdentityGroup(root, titleText, identities, emptyText, actionFactory) {
    var group = _channelHubAdminNode('section', 'channel-host-admin-section');
    group.appendChild(_channelHubAdminNode('h4', '', titleText));
    if (!identities || !identities.length) {
        group.appendChild(_channelHubAdminNode('p', 'channel-host-admin-muted', emptyText));
    } else {
        var list = _channelHubAdminNode('div', 'channel-host-admin-identity-list');
        identities.forEach(function(identity) {
            list.appendChild(_channelHubAdminIdentityRow(
                identity,
                '',
                '',
                actionFactory ? actionFactory(identity) : null
            ));
        });
        group.appendChild(list);
    }
    root.appendChild(group);
}

function _channelHubRenderAdminAccess(root, admin, refreshHandler, actions) {
    root.textContent = '';
    var mutationDisabled = !admin.running || !actions || !!actions.disabled;
    _channelHubAdminHeader(
        root,
        'Access',
        'Hub-wide and per-channel authority \u00b7 ' + _channelHubAdminGeneratedLabel(admin),
        refreshHandler,
        actions ? [{
            label: 'Change access',
            primary: true,
            disabled: mutationDisabled,
            title: mutationDisabled && !admin.running
                ? 'Start the hub before changing access'
                : '',
            handler: actions.manageAccess
        }] : null
    );
    if (!admin.running) {
        root.appendChild(_channelHubAdminNotice(
            'neutral',
            'Saved access is read-only while stopped',
            'Start the hub to grant, revoke, invite, kick, or ban an identity.'
        ));
    }
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
        'No identities are banned from this hub.',
        actions ? function(identity) {
            return [{
                label: 'Review',
                disabled: mutationDisabled,
                handler: function() {
                    actions.manageAccess({
                        targetIdentity: identity,
                        scope: 'hub'
                    });
                }
            }];
        } : null
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
        if (actions) {
            var roomActions = _channelHubAdminNode('div', 'channel-host-admin-access-room-actions');
            roomActions.appendChild(_channelHubAdminButton(
                room.registered ? 'Change channel access' : 'Manage live member',
                function() {
                    actions.manageAccess({ scope: 'room:' + room.name });
                },
                {
                    disabled: mutationDisabled,
                    title: mutationDisabled && !admin.running
                        ? 'Start the hub before changing access'
                        : ''
                }
            ));
            body.appendChild(roomActions);
        }
        [
            ['Operators', room.operators || []],
            ['Voiced identities', room.voiced || []],
            ['Channel bans', room.bans || []]
        ].forEach(function(group, groupIndex) {
            var block = _channelHubAdminNode('div', 'channel-host-admin-access-group');
            block.appendChild(_channelHubAdminNode('strong', '', group[0]));
            if (!group[1].length) {
                block.appendChild(_channelHubAdminNode('span', '', 'None'));
            } else {
                group[1].forEach(function(identity) {
                    block.appendChild(_channelHubAdminIdentityRow(
                        identity,
                        '',
                        '',
                        actions ? [{
                            label: 'Review',
                            disabled: mutationDisabled,
                            handler: function() {
                                actions.manageAccess({
                                    targetIdentity: identity,
                                    scope: 'room:' + room.name,
                                    choiceId: groupIndex === 0
                                        ? 'room_operator'
                                        : (groupIndex === 1 ? 'room_voice' : 'room_ban')
                                });
                            }
                        }] : null
                    ));
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
                    expiry ? 'Expires ' + expiry.toLocaleString() : 'Expiry unavailable',
                    actions ? [{
                        label: 'Review',
                        disabled: mutationDisabled,
                        handler: function() {
                            actions.manageAccess({
                                targetIdentity: invitation.identity_hash,
                                scope: 'room:' + room.name,
                                choiceId: 'room_invitation'
                            });
                        }
                    }] : null
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
    var retention = Number(policy.retention_secs) || 0;
    var retentionHours = Math.round(retention / 3600);
    _channelHubAdminHeader(
        root,
        'Activity',
        (retention
            ? 'Last ' + retentionHours + ' ' + (retentionHours === 1 ? 'hour' : 'hours') +
                ' \u00b7 Memory only'
            : 'Off') + ' \u00b7 ' +
            _channelHubAdminGeneratedLabel(admin),
        refreshHandler
    );
    if (!retention) {
        root.appendChild(_channelHubAdminEmpty(
            'Recent activity is off',
            'Enable it in Settings when you need a temporary moderation view.'
        ));
        return;
    }
    if (!admin.running) {
        root.appendChild(_channelHubAdminEmpty(
            'No activity while stopped',
            'Recent activity clears whenever the hub stops.'
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
            'No recent activity',
            'Nothing to review yet.'
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

function _channelHubAdminUtf8Length(value) {
    if (typeof RS !== 'undefined' && RS.text && typeof RS.text.utf8Length === 'function') {
        return RS.text.utf8Length(value);
    }
    value = String(value || '');
    var bytes = 0;
    for (var index = 0; index < value.length; index++) {
        var code = value.charCodeAt(index);
        if (code < 0x80) bytes += 1;
        else if (code < 0x800) bytes += 2;
        else if (code >= 0xd800 && code <= 0xdbff &&
                index + 1 < value.length &&
                value.charCodeAt(index + 1) >= 0xdc00 &&
                value.charCodeAt(index + 1) <= 0xdfff) {
            bytes += 4;
            index += 1;
        } else {
            bytes += 3;
        }
    }
    return bytes;
}

function _channelHubAdminIdentityValue(value) {
    var identity = String(value || '').trim().toLowerCase();
    return /^[0-9a-f]{32}$/.test(identity) ? identity : '';
}

function _channelHubAdminRoom(admin, roomName) {
    var rooms = admin && Array.isArray(admin.rooms) ? admin.rooms : [];
    for (var index = 0; index < rooms.length; index++) {
        if (String(rooms[index].name) === String(roomName)) return rooms[index];
    }
    return null;
}

function _channelHubAdminPerson(admin, identityHash) {
    var people = admin && Array.isArray(admin.people) ? admin.people : [];
    for (var index = 0; index < people.length; index++) {
        if (String(people[index].identity_hash).toLowerCase() === identityHash) {
            return people[index];
        }
    }
    return null;
}

function _channelHubAdminHasIdentity(values, identityHash) {
    return (values || []).some(function(value) {
        return String(value).toLowerCase() === identityHash;
    });
}

function _channelHubAdminKeyModeOptions(registered, fixedLiveName, keyConfigured) {
    if (registered && keyConfigured) {
        return [
            { value: 'keep', label: 'Keep existing key' },
            { value: 'set', label: 'Replace key' },
            { value: 'clear', label: 'Remove key' }
        ];
    }
    if (fixedLiveName && keyConfigured) {
        return [
            { value: 'set', label: 'Replace key while registering' },
            { value: 'clear', label: 'Remove key while registering' }
        ];
    }
    return [
        {
            value: 'keep',
            label: registered ? 'No key' : 'Create without a key'
        },
        {
            value: 'set',
            label: registered ? 'Set a key' : 'Create with a key'
        }
    ];
}

function _channelHubAdminAccessChoices(admin, targetValue, scope) {
    var targetIdentity = _channelHubAdminIdentityValue(targetValue);
    if (!targetIdentity) return [];
    var serverOperator = _channelHubAdminHasIdentity(
        admin && admin.server_operators,
        targetIdentity
    );
    var choices = [];
    if (serverOperator) return choices;
    if (scope === 'hub') {
        var hubBanned = _channelHubAdminHasIdentity(admin && admin.hub_bans, targetIdentity);
        choices.push({
            id: 'hub_ban',
            label: hubBanned ? 'Remove hub ban' : 'Ban from hub',
            detail: hubBanned
                ? 'Allow this identity to connect to the hub again.'
                : 'Block this identity and close every current session.',
            mutation: {
                action: 'set_hub_ban',
                target_identity: targetIdentity,
                banned: !hubBanned
            },
            toast: hubBanned ? 'Hub ban removed' : 'Identity banned from hub',
            confirmation: hubBanned ? null : {
                title: 'Ban identity from this hub?',
                message: 'Ban ' + targetIdentity +
                    '? Every live session for this identity will close, and future hub access will be blocked.',
                confirmText: 'Ban from hub',
                danger: true
            }
        });
        return choices;
    }

    if (String(scope || '').indexOf('room:') !== 0) return choices;
    var roomName = String(scope).slice(5);
    var room = _channelHubAdminRoom(admin, roomName);
    if (!room) return choices;
    var roomLabel = '#' + roomName;
    var person = _channelHubAdminPerson(admin, targetIdentity);
    var inRoom = !!(person && (person.rooms || []).some(function(name) {
        return String(name) === roomName;
    }));

    if (room.registered) {
        if (!serverOperator) {
            var operator = _channelHubAdminHasIdentity(room.operators, targetIdentity);
            choices.push({
                id: 'room_operator',
                label: operator ? 'Remove channel operator' : 'Grant channel operator',
                detail: operator
                    ? 'Remove channel policy and moderation authority.'
                    : 'Allow this identity to manage channel policy and moderation.',
                mutation: {
                    action: 'set_room_role',
                    room: roomName,
                    target_identity: targetIdentity,
                    role: 'operator',
                    enabled: !operator
                },
                toast: operator ? 'Channel operator removed' : 'Channel operator granted',
                confirmation: {
                    title: operator ? 'Remove operator authority?' : 'Grant operator authority?',
                    message: (operator ? 'Remove' : 'Grant') + ' operator authority for ' +
                        targetIdentity + ' in ' + roomLabel + '?',
                    confirmText: operator ? 'Remove operator' : 'Grant operator',
                    danger: operator
                }
            });
        }

        var voiced = _channelHubAdminHasIdentity(room.voiced, targetIdentity);
        choices.push({
            id: 'room_voice',
            label: voiced ? 'Remove voice' : 'Grant voice',
            detail: voiced
                ? 'In a moderated channel, this identity will no longer be able to post.'
                : 'Allow this identity to post while the channel is moderated.',
            mutation: {
                action: 'set_room_role',
                room: roomName,
                target_identity: targetIdentity,
                role: 'voice',
                enabled: !voiced
            },
            toast: voiced ? 'Voice removed' : 'Voice granted',
            confirmation: voiced ? {
                title: 'Remove voice?',
                message: 'Remove voice for ' + targetIdentity + ' in ' + roomLabel +
                    '? If the channel is moderated, they will no longer be able to post.',
                confirmText: 'Remove voice',
                danger: true
            } : null
        });

        if (!serverOperator) {
            var roomBanned = _channelHubAdminHasIdentity(room.bans, targetIdentity);
            choices.push({
                id: 'room_ban',
                label: roomBanned ? 'Remove channel ban' : 'Ban from channel',
                detail: roomBanned
                    ? 'Allow this identity to join the channel again.'
                    : 'Remove every current room session and block rejoining.',
                mutation: {
                    action: 'set_room_ban',
                    room: roomName,
                    target_identity: targetIdentity,
                    banned: !roomBanned
                },
                toast: roomBanned ? 'Channel ban removed' : 'Identity banned from channel',
                confirmation: roomBanned ? null : {
                    title: 'Ban identity from this channel?',
                    message: 'Ban ' + targetIdentity + ' from ' + roomLabel +
                        '? Every current session will leave the channel, and rejoining will be blocked.',
                    confirmText: 'Ban from channel',
                    danger: true
                }
            });

            var invited = (room.invitations || []).some(function(invitation) {
                return String(invitation.identity_hash).toLowerCase() === targetIdentity;
            });
            var gated = !!(room.modes &&
                (room.modes.invite_only || room.modes.join_key_configured));
            if (invited || gated) {
                choices.push({
                    id: 'room_invitation',
                    label: invited ? 'Revoke invitation' : 'Invite to channel',
                    detail: invited
                        ? 'Remove the active invitation and any reconnect lease.'
                        : (room.modes && room.modes.join_key_configured
                            ? 'Temporarily allow joining without the channel key.'
                            : 'Temporarily allow joining this invite-only channel.'),
                    mutation: {
                        action: 'set_invitation',
                        room: roomName,
                        target_identity: targetIdentity,
                        invited: !invited
                    },
                    toast: invited ? 'Invitation revoked' : 'Invitation created',
                    confirmation: invited ? {
                        title: 'Revoke invitation?',
                        message: 'Revoke the invitation for ' + targetIdentity + ' in ' +
                            roomLabel + '? Its active reconnect lease will also be removed.',
                        confirmText: 'Revoke invitation',
                        danger: true
                    } : null
                });
            }
        }
    }

    if (inRoom && !serverOperator) {
        choices.push({
            id: 'room_kick',
            label: 'Remove live member',
            detail: 'Remove every current session from this channel without creating a ban.',
            mutation: {
                action: 'kick',
                room: roomName,
                target_identity: targetIdentity
            },
            toast: 'Identity removed from channel',
            confirmation: {
                title: 'Remove live member?',
                message: 'Remove every live session for ' + targetIdentity + ' from ' +
                    roomLabel + '? They may rejoin unless separately banned.',
                confirmText: 'Remove from channel',
                danger: true
            }
        });
    }
    return choices;
}

function _channelHubAdminDismissChildren() {
    var dismisses = _channelHubAdminChildDismisses.slice().reverse();
    _channelHubAdminChildDismisses = [];
    dismisses.forEach(function(dismiss) { dismiss(); });
}

function _channelHubAdminBuildChildSheet(options, onClose) {
    var built = null;
    built = _rsBuildSheet(options || {}, function(value) {
        _channelHubAdminChildDismisses = _channelHubAdminChildDismisses.filter(
            function(dismiss) { return dismiss !== built.dismiss; }
        );
        if (onClose) onClose(value);
    });
    _channelHubAdminChildDismisses.push(built.dismiss);
    return built;
}

function _channelHubAdminConfirm(options) {
    options = options || {};
    return new Promise(function(resolve) {
        var built = _channelHubAdminBuildChildSheet(
            { title: options.title || 'Confirm change' },
            function(value) { resolve(!!value); }
        );
        var message = _channelHubAdminNode(
            'p',
            'channel-sheet-copy channel-host-admin-confirm-copy',
            options.message || 'Apply this change?'
        );
        built.body.appendChild(message);
        var cancel = _channelHubAdminButton('Cancel', function() {
            built.dismiss(false);
        });
        var confirm = _channelHubAdminButton(
            options.confirmText || 'Confirm',
            function() { built.dismiss(true); },
            { primary: !options.danger, danger: !!options.danger }
        );
        built.footer.appendChild(cancel);
        built.footer.appendChild(confirm);
        _channelsPresentSheet(built, confirm);
    });
}

function _channelHubOpenChannelEditor(admin, existingRoom, mutationHandler) {
    if (!admin || !admin.running || typeof _rsBuildSheet !== 'function') return;
    var registered = !!(existingRoom && existingRoom.registered);
    var creating = !registered;
    var fixedLiveName = !!(existingRoom && !existingRoom.registered);
    var modes = existingRoom && existingRoom.modes || {};
    var keyInput = null;
    var secretMutation = null;
    var closed = false;
    var busy = false;
    var built = _channelHubAdminBuildChildSheet({
        title: registered ? 'Manage channel' : (fixedLiveName ? 'Register channel' : 'Create channel')
    }, function() {
        closed = true;
        if (keyInput) keyInput.value = '';
        // RS.invoke may still be waiting for the native bridge. Keep its
        // in-flight argument intact until the promise settles below.
        if (!busy && secretMutation && secretMutation.join_key !== undefined) {
            secretMutation.join_key = '';
        }
        if (!busy) secretMutation = null;
    });
    built.sheet.classList.add('channel-host-admin-edit-sheet');
    built.body.classList.add('channel-host-admin-edit-body');

    built.body.appendChild(_channelHubAdminNode(
        'p',
        'channel-sheet-copy',
        registered
            ? 'Edit one complete durable policy projection for #' + existingRoom.name + '.'
            : (fixedLiveName
                ? 'Turn the current live-only channel into durable policy and access.'
                : 'Create durable policy without storing conversation history or a roster.')
    ));

    var roomInput = _channelHubAdminNode('input', 'nr-input-sm');
    roomInput.type = 'text';
    roomInput.maxLength = 64;
    roomInput.autocomplete = 'off';
    roomInput.setAttribute('autocorrect', 'off');
    roomInput.setAttribute('autocapitalize', 'none');
    roomInput.setAttribute('spellcheck', 'false');
    roomInput.placeholder = 'field-ops';
    roomInput.value = existingRoom ? String(existingRoom.name || '') : '';
    roomInput.disabled = registered || fixedLiveName;
    built.body.appendChild(_channelHubField(
        'Channel name',
        roomInput,
        registered || fixedLiveName
            ? 'Channel names cannot be renamed.'
            : 'Shown with # in the channel list · 64 UTF-8 bytes maximum'
    ));

    var topicInput = _channelHubAdminNode('textarea', 'nr-input-sm channel-host-admin-topic');
    topicInput.rows = 3;
    topicInput.maxLength = Number(admin.limits && admin.limits.max_message_body_bytes) || 350;
    topicInput.placeholder = 'What is this channel for?';
    topicInput.value = existingRoom ? String(existingRoom.topic || '') : '';
    built.body.appendChild(_channelHubField(
        'Topic',
        topicInput,
        'Empty clears the topic · ' +
            (Number(admin.limits && admin.limits.max_message_body_bytes) || 350) +
            ' UTF-8 bytes maximum'
    ));

    var policySection = _channelHubAdminNode(
        'section',
        'channel-host-admin-editor-section'
    );
    policySection.appendChild(_channelHubAdminNode('h3', '', 'Channel policy'));
    var privateToggle = _channelHubToggle(
        'Private channel',
        'Hide existence from people who are neither members nor operators',
        !!modes.private
    );
    var inviteToggle = _channelHubToggle(
        'Invite only',
        'Require an active invitation or reconnect lease to join',
        !!modes.invite_only
    );
    var moderatedToggle = _channelHubToggle(
        'Moderated posting',
        'Only operators and voiced identities can post',
        !!modes.moderated
    );
    var outsideToggle = _channelHubToggle(
        'Members post',
        'Reject room messages from identities that have not joined',
        !!modes.no_outside_messages
    );
    var topicOpsToggle = _channelHubToggle(
        'Operator topics',
        'Only channel or hub operators can change the topic',
        !!modes.topic_operators_only
    );
    [
        privateToggle,
        inviteToggle,
        moderatedToggle,
        outsideToggle,
        topicOpsToggle
    ].forEach(function(toggle) {
        policySection.appendChild(toggle.row);
    });
    built.body.appendChild(policySection);

    var keySection = _channelHubAdminNode(
        'section',
        'channel-host-admin-editor-section'
    );
    keySection.appendChild(_channelHubAdminNode('h3', '', 'Join key'));
    var keyMode = _channelHubAdminNode('select', 'nr-select channel-host-admin-key-select');
    function addKeyMode(value, label) {
        var option = _channelHubAdminNode('option', '', label);
        option.value = value;
        keyMode.appendChild(option);
    }
    _channelHubAdminKeyModeOptions(
        registered,
        fixedLiveName,
        !!modes.join_key_configured
    ).forEach(function(option) {
        addKeyMode(option.value, option.label);
    });
    keySection.appendChild(_channelHubField(
        registered ? 'Key change' : 'Key protection',
        keyMode,
        modes.join_key_configured
            ? 'The existing key cannot be recovered. Replacing it invalidates the old key.'
            : 'Keys are converted to verify-only salted digests and never recoverable.'
    ));

    keyInput = _channelHubAdminNode('input', 'nr-input-sm channel-host-admin-secret');
    keyInput.type = 'password';
    keyInput.minLength = 8;
    keyInput.maxLength = 128;
    keyInput.autocomplete = 'new-password';
    keyInput.setAttribute('autocorrect', 'off');
    keyInput.setAttribute('autocapitalize', 'none');
    keyInput.setAttribute('spellcheck', 'false');
    var keyField = _channelHubField(
        registered && modes.join_key_configured ? 'New join key' : 'Join key',
        keyInput,
        '8–128 UTF-8 bytes · whitespace is not allowed · cleared after submission'
    );
    keyField.hidden = true;
    keySection.appendChild(keyField);
    built.body.appendChild(keySection);

    var error = _channelHubAdminNode('div', 'channel-sheet-error channel-host-error');
    error.setAttribute('aria-live', 'polite');
    built.body.appendChild(error);

    var unregister = null;
    if (registered) {
        unregister = _channelHubAdminButton(
            'Unregister',
            function() {
                _channelHubAdminConfirm({
                    title: 'Unregister channel?',
                    message: 'Remove the saved policy and access lists for #' +
                        existingRoom.name +
                        '? Current members remain until they leave, but the channel becomes live-only and will disappear when empty.',
                    confirmText: 'Unregister channel',
                    danger: true
                }).then(function(confirmed) {
                    if (!confirmed || closed) return;
                    keyInput.value = '';
                    setBusy(true);
                    error.textContent = '';
                    return mutationHandler({
                        action: 'unregister_channel',
                        room: existingRoom.name
                    }, 'Channel unregistered').then(function(nextAdmin) {
                        if (nextAdmin && !closed) built.dismiss(true);
                    }).catch(function(mutationError) {
                        if (!closed) {
                            error.textContent = (mutationError && mutationError.message) ||
                                'Could not unregister this channel.';
                            if (mutationError && mutationError.code === 'registry_unavailable') {
                                cancel.disabled = false;
                                cancel.textContent = 'Close and review';
                                submit.hidden = true;
                                unregister.disabled = true;
                            } else {
                                setBusy(false);
                            }
                        }
                    });
                });
            },
            { danger: true }
        );
        unregister.classList.add('channel-host-admin-footer-danger');
        built.footer.appendChild(unregister);
    }

    var cancel = _channelHubAdminButton('Cancel', function() { built.dismiss(); });
    var submit = _channelHubAdminButton(
        registered ? 'Save channel' : (fixedLiveName ? 'Register channel' : 'Create channel'),
        submitChannel,
        { primary: true }
    );
    built.footer.appendChild(cancel);
    built.footer.appendChild(submit);

    var mutableControls = [
        topicInput,
        keyMode,
        keyInput,
        privateToggle.input,
        inviteToggle.input,
        moderatedToggle.input,
        outsideToggle.input,
        topicOpsToggle.input
    ];
    if (!registered && !fixedLiveName) mutableControls.push(roomInput);

    function setBusy(value) {
        busy = value;
        mutableControls.forEach(function(control) { control.disabled = value; });
        roomInput.disabled = value || registered || fixedLiveName;
        cancel.disabled = value;
        submit.disabled = value;
        if (unregister) unregister.disabled = value;
        submit.textContent = value
            ? 'Applying…'
            : (registered ? 'Save channel' : (fixedLiveName ? 'Register channel' : 'Create channel'));
    }

    function renderKeyMode() {
        keyField.hidden = keyMode.value !== 'set';
        if (keyField.hidden) keyInput.value = '';
    }

    function roomPolicy() {
        return {
            invite_only: !!inviteToggle.input.checked,
            moderated: !!moderatedToggle.input.checked,
            no_outside_messages: !!outsideToggle.input.checked,
            private: !!privateToggle.input.checked,
            topic_operators_only: !!topicOpsToggle.input.checked
        };
    }

    function validateAndBuildMutation() {
        var roomName = roomInput.value.trim().toLowerCase();
        if (!roomName) throw new Error('Choose a channel name.');
        if (_channelHubAdminUtf8Length(roomName) > 64) {
            throw new Error('Channel name cannot exceed 64 UTF-8 bytes.');
        }
        var topic = topicInput.value.trim();
        var topicLimit = Number(admin.limits && admin.limits.max_message_body_bytes) || 350;
        if (_channelHubAdminUtf8Length(topic) > topicLimit) {
            throw new Error('Topic cannot exceed ' + topicLimit + ' UTF-8 bytes.');
        }
        if (/[\u0000-\u001f\u007f-\u009f]/.test(topic)) {
            throw new Error('Topic cannot contain control characters.');
        }
        var mutation = {
            action: registered ? 'update_channel' : 'create_channel',
            room: roomName,
            topic: topic,
            policy: roomPolicy()
        };
        if (keyMode.value === 'set') {
            var key = keyInput.value;
            var keyBytes = _channelHubAdminUtf8Length(key);
            if (keyBytes < 8 || keyBytes > 128) {
                throw new Error('Join key must be 8–128 UTF-8 bytes.');
            }
            if (/\s/.test(key) || /[\u0000-\u001f\u007f-\u009f]/.test(key)) {
                throw new Error('Join key cannot contain whitespace or control characters.');
            }
            mutation.join_key = key;
        } else if (registered && keyMode.value === 'clear') {
            mutation.clear_join_key = true;
        }
        return mutation;
    }

    function channelConfirmation(mutation) {
        if (!existingRoom) return null;
        var consequences = [];
        [
            ['private', 'hide the channel from non-members'],
            ['invite_only', 'require invitations to join'],
            ['moderated', 'limit posting to operators and voiced identities'],
            ['no_outside_messages', 'require membership before posting'],
            ['topic_operators_only', 'limit topic changes to operators']
        ].forEach(function(entry) {
            if (!modes[entry[0]] && mutation.policy[entry[0]]) {
                consequences.push(entry[1]);
            }
        });
        if (mutation.join_key !== undefined) {
            consequences.push(modes.join_key_configured
                ? 'replace the join key and invalidate the old key'
                : 'require a join key');
        }
        if (mutation.clear_join_key ||
                (!registered && modes.join_key_configured && keyMode.value === 'clear')) {
            consequences.push('remove join-key protection');
        }
        if (!consequences.length) return null;
        return {
            title: 'Apply access-policy changes?',
            message: (registered ? 'Update #' : 'Register #') + existingRoom.name + ' to ' +
                consequences.join(', ') + '?',
            confirmText: 'Apply changes',
            danger: true
        };
    }

    function clearSubmittedSecret(mutation) {
        keyInput.value = '';
        if (mutation && mutation.join_key !== undefined) mutation.join_key = '';
        if (secretMutation === mutation) secretMutation = null;
    }

    function submitChannel() {
        if (busy || closed) return;
        error.textContent = '';
        var mutation;
        try {
            mutation = validateAndBuildMutation();
        } catch (validationError) {
            error.textContent = validationError.message;
            return;
        }
        var confirmation = channelConfirmation(mutation);
        var confirmed = confirmation
            ? _channelHubAdminConfirm(confirmation)
            : Promise.resolve(true);
        confirmed.then(function(allowed) {
            if (!allowed || closed) return;
            setBusy(true);
            secretMutation = mutation;
            return mutationHandler(
                mutation,
                registered ? 'Channel updated' : 'Channel created'
            ).then(function(nextAdmin) {
                clearSubmittedSecret(mutation);
                if (nextAdmin && !closed) built.dismiss(true);
                else if (!closed) setBusy(false);
            }).catch(function(mutationError) {
                clearSubmittedSecret(mutation);
                if (!closed) {
                    error.textContent = (mutationError && mutationError.message) ||
                        'Could not apply this channel policy.';
                    if (mutationError && mutationError.code === 'registry_unavailable') {
                        cancel.disabled = false;
                        cancel.textContent = 'Close and review';
                        submit.hidden = true;
                        if (unregister) unregister.disabled = true;
                    } else {
                        setBusy(false);
                    }
                }
            });
        });
    }

    keyMode.addEventListener('change', renderKeyMode);
    renderKeyMode();
    _channelsPresentSheet(built, registered || fixedLiveName ? topicInput : roomInput);
}

function _channelHubOpenAccessEditor(admin, options, mutationHandler) {
    if (!admin || !admin.running || typeof _rsBuildSheet !== 'function') return;
    options = options || {};
    var closed = false;
    var busy = false;
    var preferredChoice = options.choiceId || '';
    var built = _channelHubAdminBuildChildSheet(
        { title: 'Change access' },
        function() { closed = true; }
    );
    built.sheet.classList.add('channel-host-admin-edit-sheet');
    built.body.classList.add('channel-host-admin-edit-body');
    built.body.appendChild(_channelHubAdminNode(
        'p',
        'channel-sheet-copy',
        'Choose one explicit authority or moderation change. Complete identity hashes are required.'
    ));

    var targetInput = _channelHubAdminNode('input', 'nr-input-sm mono');
    targetInput.type = 'text';
    targetInput.maxLength = 64;
    targetInput.autocomplete = 'off';
    targetInput.setAttribute('autocorrect', 'off');
    targetInput.setAttribute('autocapitalize', 'none');
    targetInput.setAttribute('spellcheck', 'false');
    targetInput.placeholder = '32-character identity hash';
    targetInput.value = String(options.targetIdentity || '');
    built.body.appendChild(_channelHubField(
        'Target identity',
        targetInput,
        'Use the complete 32-character hexadecimal identity hash'
    ));

    var scopeSelect = _channelHubAdminNode(
        'select',
        'nr-select channel-host-admin-scope-select'
    );
    var hubOption = _channelHubAdminNode('option', '', 'Hub-wide access');
    hubOption.value = 'hub';
    scopeSelect.appendChild(hubOption);
    (admin.rooms || []).forEach(function(room) {
        var option = _channelHubAdminNode(
            'option',
            '',
            '#' + room.name + (room.registered ? ' · saved' : ' · live only')
        );
        option.value = 'room:' + room.name;
        scopeSelect.appendChild(option);
    });
    var desiredScope = String(options.scope || '');
    if (!desiredScope && options.person && (options.person.rooms || []).length) {
        desiredScope = 'room:' + options.person.rooms[0];
    }
    scopeSelect.value = desiredScope || 'hub';
    if (!scopeSelect.value) scopeSelect.value = 'hub';
    built.body.appendChild(_channelHubField(
        'Scope',
        scopeSelect,
        'Saved roles and access require a registered channel; live-only channels support removal only'
    ));

    var actionSelect = _channelHubAdminNode(
        'select',
        'nr-select channel-host-admin-scope-select'
    );
    var actionHint = _channelHubAdminNode(
        'span',
        'channel-host-field-hint',
        'Enter a complete identity to see available changes.'
    );
    var actionField = _channelHubField('Change', actionSelect);
    actionField.appendChild(actionHint);
    built.body.appendChild(actionField);

    var protectedNotice = _channelHubAdminNotice(
        'warning',
        'Hub operator protection',
        'Hub operators cannot be deopped, kicked, or banned. Server-operator authority is configured outside this view.'
    );
    protectedNotice.hidden = true;
    built.body.appendChild(protectedNotice);

    var error = _channelHubAdminNode('div', 'channel-sheet-error channel-host-error');
    error.setAttribute('aria-live', 'polite');
    built.body.appendChild(error);

    var cancel = _channelHubAdminButton('Cancel', function() { built.dismiss(); });
    var submit = _channelHubAdminButton('Apply change', submitAccess, { primary: true });
    built.footer.appendChild(cancel);
    built.footer.appendChild(submit);

    function currentChoices() {
        return _channelHubAdminAccessChoices(
            admin,
            targetInput.value,
            scopeSelect.value
        );
    }

    function selectedChoice() {
        var choiceId = actionSelect.value;
        var choices = currentChoices();
        for (var index = 0; index < choices.length; index++) {
            if (choices[index].id === choiceId) return choices[index];
        }
        return null;
    }

    function renderChoices() {
        var previous = actionSelect.value || preferredChoice;
        preferredChoice = '';
        actionSelect.textContent = '';
        var identity = _channelHubAdminIdentityValue(targetInput.value);
        var choices = currentChoices();
        if (!choices.length) {
            var unavailable = _channelHubAdminNode(
                'option',
                '',
                identity ? 'No available change for this scope' : 'Enter a complete identity'
            );
            unavailable.value = '';
            unavailable.disabled = true;
            actionSelect.appendChild(unavailable);
            actionSelect.value = '';
        } else {
            choices.forEach(function(choice) {
                var option = _channelHubAdminNode('option', '', choice.label);
                option.value = choice.id;
                actionSelect.appendChild(option);
            });
            var preferredExists = choices.some(function(choice) {
                return choice.id === previous;
            });
            actionSelect.value = preferredExists ? previous : choices[0].id;
        }
        var choice = selectedChoice();
        actionHint.textContent = choice
            ? choice.detail
            : (identity
                ? 'This identity is protected or the selected channel has no applicable durable action.'
                : 'Enter a complete 32-character hexadecimal identity hash.');
        var protectedIdentity = identity &&
            _channelHubAdminHasIdentity(admin.server_operators, identity);
        protectedNotice.hidden = !protectedIdentity;
        submit.disabled = busy || !choice;
        submit.className = choice && choice.confirmation && choice.confirmation.danger
            ? 'nr-btn nr-btn-danger channel-host-admin-action'
            : 'nr-btn nr-btn-primary channel-host-admin-action';
        submit.textContent = busy ? 'Applying…' : (choice ? choice.label : 'Apply change');
    }

    function setBusy(value) {
        busy = value;
        targetInput.disabled = value;
        scopeSelect.disabled = value;
        actionSelect.disabled = value;
        cancel.disabled = value;
        renderChoices();
    }

    function submitAccess() {
        if (busy || closed) return;
        error.textContent = '';
        var identity = _channelHubAdminIdentityValue(targetInput.value);
        if (!identity) {
            error.textContent = 'Enter a complete 32-character hexadecimal identity hash.';
            return;
        }
        targetInput.value = identity;
        var choice = selectedChoice();
        if (!choice) {
            error.textContent = 'No applicable access change is available.';
            return;
        }
        var confirmed = choice.confirmation
            ? _channelHubAdminConfirm(choice.confirmation)
            : Promise.resolve(true);
        confirmed.then(function(allowed) {
            if (!allowed || closed) return;
            setBusy(true);
            return mutationHandler(choice.mutation, choice.toast).then(function(nextAdmin) {
                if (nextAdmin && !closed) built.dismiss(true);
                else if (!closed) setBusy(false);
            }).catch(function(mutationError) {
                if (!closed) {
                    error.textContent = (mutationError && mutationError.message) ||
                        'Could not apply this access change.';
                    if (mutationError && mutationError.code === 'registry_unavailable') {
                        cancel.disabled = false;
                        cancel.textContent = 'Close and review';
                        submit.hidden = true;
                    } else {
                        setBusy(false);
                    }
                }
            });
        });
    }

    targetInput.addEventListener('input', renderChoices);
    targetInput.addEventListener('change', function() {
        var identity = _channelHubAdminIdentityValue(targetInput.value);
        if (identity) targetInput.value = identity;
        renderChoices();
    });
    scopeSelect.addEventListener('change', renderChoices);
    actionSelect.addEventListener('change', renderChoices);
    renderChoices();
    _channelsPresentSheet(built, options.targetIdentity ? actionSelect : targetInput);
}

function channelHubOpenManager(initialOverview) {
    if (typeof _rsBuildSheet !== 'function') return;
    _channelHubAdminDismissChildren();
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
    if (!_channelHubHostingEnabled(overview)) {
        if (typeof showToast === 'function') {
            showToast('Turn on Channel hosting in Settings first', 'toast-orange', 3200);
        }
        return;
    }

    var identityGeneration = _channelHubIdentityGeneration;
    var built = _rsBuildSheet({ title: 'Hub administration' }, function() {
        if (_channelHubManagerSequence !== sequence) return;
        _channelHubAdminDismissChildren();
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
    var addressValueRow = document.createElement('div');
    addressValueRow.className = 'channel-host-address-value-row';
    var addressValue = document.createElement('code');
    addressCopy.appendChild(addressLabel);
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
    addressValueRow.appendChild(addressValue);
    addressValueRow.appendChild(copyAddress);
    addressCopy.appendChild(addressValueRow);
    address.appendChild(addressCopy);
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
        'Hub profile, discovery, and moderation'
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
    greetingInput.placeholder = 'Welcome people, share rules, and tell them where to begin';
    greetingInput.value = settings.greeting || '';
    var greetingField = _channelHubField(
        'Welcome & guidance',
        greetingInput,
        'Shown after WELCOME when someone connects'
    );
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
    discoveryHint.textContent = 'At startup and on this schedule, so nearby people can find it';
    discoveryCopy.appendChild(discoveryLabel);
    discoveryCopy.appendChild(discoveryHint);
    var announceInput = document.createElement('select');
    announceInput.className = 'nr-select channel-host-announce-select';
    [
        [900, 'Every 15 minutes'],
        [1800, 'Every 30 minutes'],
        [3600, 'Every hour'],
        [43200, 'Every 12 hours'],
        [86400, 'Every 24 hours']
    ].forEach(function(optionValue) {
        var option = document.createElement('option');
        option.value = String(optionValue[0]);
        option.textContent = optionValue[1];
        announceInput.appendChild(option);
    });
    announceInput.value = String(Number(settings.announce_interval_secs) || 900);
    discoveryLabel.htmlFor = 'channel-host-announce-' + sequence;
    announceInput.id = discoveryLabel.htmlFor;
    discoveryRow.appendChild(discoveryCopy);
    discoveryRow.appendChild(announceInput);
    discovery.appendChild(discoveryRow);
    panels.settings.appendChild(discovery);

    var moderation = document.createElement('section');
    moderation.className = 'channel-host-section';
    var moderationTitle = document.createElement('h3');
    moderationTitle.textContent = 'Moderation';
    moderation.appendChild(moderationTitle);
    var recentActivityRow = document.createElement('div');
    recentActivityRow.className = 'channel-host-discovery-row';
    var recentActivityCopy = document.createElement('div');
    recentActivityCopy.className = 'channel-host-discovery-copy';
    var recentActivityLabel = document.createElement('label');
    recentActivityLabel.textContent = 'Recent activity';
    var recentActivityHint = document.createElement('span');
    recentActivityHint.textContent = 'Temporary moderation context, held only in memory';
    recentActivityCopy.appendChild(recentActivityLabel);
    recentActivityCopy.appendChild(recentActivityHint);
    var recentActivityInput = document.createElement('select');
    recentActivityInput.className = 'nr-select channel-host-recent-activity-select';
    [
        [0, 'OFF'],
        [3600, '1 hour'],
        [21600, '6 hours'],
        [43200, '12 hours'],
        [86400, '24 hours']
    ].forEach(function(optionValue) {
        var option = document.createElement('option');
        option.value = String(optionValue[0]);
        option.textContent = optionValue[1];
        recentActivityInput.appendChild(option);
    });
    recentActivityInput.value = String(Number(settings.recent_activity_retention_secs) || 0);
    recentActivityLabel.htmlFor = 'channel-host-recent-activity-' + sequence;
    recentActivityInput.id = recentActivityLabel.htmlFor;
    recentActivityRow.appendChild(recentActivityCopy);
    recentActivityRow.appendChild(recentActivityInput);
    moderation.appendChild(recentActivityRow);
    panels.settings.appendChild(moderation);

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

    var controls = [
        nameInput,
        greetingInput,
        announceInput,
        recentActivityInput
    ];
    var busy = false;
    var activeTab = 'overview';
    var adminSnapshot = null;
    var adminRequest = 0;
    var adminMutationRequest = 0;
    var adminMutationBusy = false;
    var registryMutationWarning = false;
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
    }

    function managerCurrent() {
        return _channelHubManagerSequence === sequence &&
            _channelHubIdentityGeneration === identityGeneration;
    }

    function validatedAdminSnapshot(nextAdmin) {
        if (!nextAdmin || Number(nextAdmin.model_version) !== 1) {
            throw new Error('This hub admin snapshot uses an unsupported model version.');
        }
        if (!nextAdmin.evidence_policy || nextAdmin.evidence_policy.persistent !== false) {
            throw new Error('This hub admin snapshot does not satisfy the memory-only evidence contract.');
        }
        return nextAdmin;
    }

    function mutationsAvailable() {
        return managerCurrent() && !busy && !adminMutationBusy &&
            !!adminSnapshot && !!adminSnapshot.running &&
            !!(overview.status && overview.status.running);
    }

    function requireMutationsAvailable() {
        if (mutationsAvailable()) return true;
        if (typeof showToast === 'function') {
            showToast(
                adminMutationBusy
                    ? 'Another hub change is still being applied'
                    : 'Start the hub before changing channel policy or access',
                'toast-orange',
                2800
            );
        }
        return false;
    }

    function openChannelEditor(room) {
        if (!requireMutationsAvailable()) return;
        var currentRoom = room ? _channelHubAdminRoom(adminSnapshot, room.name) : null;
        _channelHubOpenChannelEditor(adminSnapshot, currentRoom, mutateAdmin);
    }

    function openAccessEditor(options) {
        if (!requireMutationsAvailable()) return;
        options = options && (
            options.targetIdentity || options.scope || options.person
        ) ? options : {};
        if (options.person && !options.targetIdentity) {
            options.targetIdentity = options.person.identity_hash;
        }
        _channelHubOpenAccessEditor(adminSnapshot, options, mutateAdmin);
    }

    function adminActions(nextAdmin) {
        return {
            disabled: busy || adminMutationBusy || !nextAdmin.running ||
                !(overview.status && overview.status.running),
            createChannel: function() { openChannelEditor(null); },
            editChannel: openChannelEditor,
            managePerson: function(person) {
                openAccessEditor({
                    person: person,
                    targetIdentity: person.identity_hash
                });
            },
            manageAccess: openAccessEditor
        };
    }

    function renderAdmin(nextAdmin) {
        var liveStatus = overview.status || {};
        var registryDegraded = !!(liveStatus.registry_degraded ||
            nextAdmin.registry_degraded ||
            (nextAdmin.rooms || []).some(function(room) { return !!room.save_pending; }));
        registryWarning.hidden = !registryDegraded;
        if (registryDegraded) {
            registryWarning.textContent = registryMutationWarning
                ? 'The live hub applied your change, but durable storage is unavailable. Saving will retry automatically; refresh to confirm.'
                : 'Some channel changes are still waiting to be saved.';
        } else {
            registryMutationWarning = false;
            registryWarning.textContent = 'Some channel changes are still waiting to be saved.';
        }
        var refreshHandler = busy || adminMutationBusy ? null : refreshAdmin;
        var actions = adminActions(nextAdmin);
        _channelHubRenderAdminOverview(panels.overview, nextAdmin, refreshHandler);
        _channelHubRenderAdminChannels(panels.channels, nextAdmin, refreshHandler, actions);
        _channelHubRenderAdminPeople(panels.people, nextAdmin, refreshHandler, actions);
        _channelHubRenderAdminAccess(panels.access, nextAdmin, refreshHandler, actions);
        _channelHubRenderAdminActivity(panels.activity, nextAdmin, refreshHandler);
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
    }

    function loadAdmin(allowDuringMutation) {
        if (adminMutationBusy && !allowDuringMutation) {
            return Promise.resolve(adminSnapshot);
        }
        var request = ++adminRequest;
        if (!adminSnapshot) renderAdminLoading();
        return RS.invoke('api_channel_hub_admin').then(function(nextAdmin) {
            if (_channelHubManagerSequence !== sequence ||
                    !managerCurrent() || request !== adminRequest) return null;
            adminSnapshot = validatedAdminSnapshot(nextAdmin);
            renderAdmin(adminSnapshot);
            return adminSnapshot;
        }).catch(function(loadError) {
            if (_channelHubManagerSequence !== sequence ||
                    !managerCurrent() || request !== adminRequest) return null;
            adminSnapshot = null;
            renderAdminError(loadError);
            return null;
        });
    }

    function refreshAdmin() {
        return loadAdmin();
    }

    function mutateAdmin(args, successText) {
        if (!mutationsAvailable()) {
            return Promise.reject(new Error(
                adminMutationBusy
                    ? 'Another hub change is still being applied.'
                    : 'Start the channel hub before making administrative changes.'
            ));
        }
        adminMutationBusy = true;
        var mutationRequest = ++adminMutationRequest;
        ++adminRequest;
        if (adminSnapshot) renderAdmin(adminSnapshot);
        renderStatus(overview);

        var request = RS.invoke('channel_hub_admin_mutate', { args: args }).then(
            function(nextAdmin) {
                if (!managerCurrent() || mutationRequest !== adminMutationRequest) {
                    return null;
                }
                adminSnapshot = validatedAdminSnapshot(nextAdmin);
                registryMutationWarning = false;
                renderAdmin(adminSnapshot);
                if (successText && typeof showToast === 'function') {
                    showToast(successText, 'toast-green', 2200);
                }
                return adminSnapshot;
            }
        ).catch(function(mutationError) {
            if (!managerCurrent() || mutationRequest !== adminMutationRequest) {
                return null;
            }
            if (mutationError && mutationError.code === 'registry_unavailable') {
                registryMutationWarning = true;
                registryWarning.hidden = false;
                registryWarning.textContent =
                    'The live hub applied your change, but durable storage is unavailable. Saving will retry automatically; refresh to confirm.';
                var pendingError = new Error(
                    'The live change was applied, but its durable save is pending. Review the warning and refresh to confirm.'
                );
                pendingError.code = 'registry_unavailable';
                return loadAdmin(true).then(function() {
                    throw pendingError;
                });
            }
            throw mutationError;
        });

        return request.then(function(result) {
            if (managerCurrent() && mutationRequest === adminMutationRequest) {
                adminMutationBusy = false;
                if (adminSnapshot) renderAdmin(adminSnapshot);
                renderStatus(overview);
            }
            return result;
        }, function(mutationError) {
            if (managerCurrent() && mutationRequest === adminMutationRequest) {
                adminMutationBusy = false;
                if (adminSnapshot) renderAdmin(adminSnapshot);
                renderStatus(overview);
            }
            throw mutationError;
        });
    }

    function currentArgs() {
        return _channelHubConfigArgs(
            nameInput,
            greetingInput,
            announceInput,
            recentActivityInput
        );
    }

    function renderDirty() {
        var dirty = !_channelHubSettingsEqual(overview.settings, currentArgs());
        save.disabled = busy || adminMutationBusy || !dirty || !nameInput.value.trim();
        save.hidden = activeTab !== 'settings';
        impact.hidden = !(dirty && overview.status && overview.status.running);
        greetingCount.textContent = (greetingInput.value.length || 0) +
            '/512 \u00b7 Use for rules and where to begin';
    }

    function setBusy(value, label) {
        busy = value;
        controls.forEach(function(control) { control.disabled = value; });
        stateButton.disabled = value || adminMutationBusy;
        close.disabled = value;
        save.disabled = value || adminMutationBusy ||
            _channelHubSettingsEqual(overview.settings, currentArgs());
        if (label) stateButton.textContent = label;
        if (adminSnapshot) renderAdmin(adminSnapshot);
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
        stateButton.disabled = busy || adminMutationBusy;
        var destination = status.destination_hash || overview.destination_hash || '';
        addressValue.textContent = destination;
        address.classList.toggle('is-empty', !destination);
        addressLabel.textContent = destination ? 'Hub address' : 'Hub address appears after the first start';
        copyAddress.hidden = !destination;
        copyAddress.disabled = !destination;
        registryWarning.hidden = !(registryMutationWarning || status.registry_degraded ||
            (adminSnapshot && adminSnapshot.registry_degraded));
        if (registryMutationWarning) {
            registryWarning.textContent =
                'The live hub applied your change, but durable storage is unavailable. Saving will retry automatically; refresh to confirm.';
        }
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
            announceInput.value = String(
                Number(updated.settings.announce_interval_secs) || 900
            );
            recentActivityInput.value = String(
                Number(updated.settings.recent_activity_retention_secs) || 0
            );
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
    _channelHubAdminDismissChildren();
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
