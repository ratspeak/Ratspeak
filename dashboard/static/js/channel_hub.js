// Desktop RRC hub hosting. Client-side Channels traffic remains in channels.js;
// this file owns only the operator surface and its stable IPC read model.

var channelHubOverview = null;
var _channelHubOverviewLoadedAt = 0;
var _channelHubOverviewPromise = null;
var _channelHubStatusRenderer = null;
var _channelHubManagerSequence = 0;

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

function _channelHubApplyOverview(overview) {
    if (!overview) return channelHubOverview;
    channelHubOverview = overview;
    _channelHubOverviewLoadedAt = Date.now();
    if (_channelHubStatusRenderer) _channelHubStatusRenderer(overview);
    return overview;
}

function channelHubLoad(force) {
    var now = Date.now();
    if (!force && channelHubOverview && now - _channelHubOverviewLoadedAt < 2000) {
        return Promise.resolve(channelHubOverview);
    }
    if (_channelHubOverviewPromise) return _channelHubOverviewPromise;
    _channelHubOverviewPromise = RS.invoke('api_channel_hub').then(function(overview) {
        return _channelHubApplyOverview(overview);
    }).then(function(overview) {
        _channelHubOverviewPromise = null;
        return overview;
    }).catch(function(error) {
        _channelHubOverviewPromise = null;
        throw error;
    });
    return _channelHubOverviewPromise;
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
        if (!overview || !overview.supported) {
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
            overview.settings && overview.settings.enabled ? 'Manage your hub' : 'Host your own',
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

function _channelHubMetric(labelText) {
    var metric = document.createElement('div');
    metric.className = 'channel-host-metric';
    var value = document.createElement('strong');
    value.textContent = '0';
    var label = document.createElement('span');
    label.textContent = labelText;
    metric.appendChild(value);
    metric.appendChild(label);
    return { root: metric, value: value };
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

function channelHubOpenManager(initialOverview) {
    if (typeof _rsBuildSheet !== 'function') return;
    var sequence = ++_channelHubManagerSequence;
    var overview = initialOverview || channelHubOverview;
    if (!overview) {
        channelHubLoad(true).then(channelHubOpenManager).catch(function(error) {
            if (typeof showToast === 'function') showToast((error && error.message) || 'Could not load your hub', 'toast-red', 3200);
        });
        return;
    }

    var built = _rsBuildSheet({ title: 'Your channel hub' }, function() {
        if (_channelHubManagerSequence === sequence) _channelHubStatusRenderer = null;
    });
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

    var metrics = document.createElement('div');
    metrics.className = 'channel-host-metrics';
    var peopleMetric = _channelHubMetric('People here');
    var roomMetric = _channelHubMetric('Channels');
    metrics.appendChild(peopleMetric.root);
    metrics.appendChild(roomMetric.root);
    built.body.appendChild(metrics);

    var registryWarning = document.createElement('div');
    registryWarning.className = 'channel-host-registry-warning';
    registryWarning.setAttribute('role', 'status');
    registryWarning.textContent = 'Some channel changes are still waiting to be saved.';
    registryWarning.hidden = true;
    built.body.appendChild(registryWarning);

    var settings = overview.settings || {};
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
    built.body.appendChild(profile);

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
    built.body.appendChild(discovery);

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
    built.body.appendChild(advanced);

    var impact = document.createElement('p');
    impact.className = 'channel-host-impact';
    impact.hidden = true;
    impact.textContent = 'Saving will briefly restart the hub so these changes can take effect.';
    built.body.appendChild(impact);

    var error = document.createElement('div');
    error.className = 'channel-sheet-error channel-host-error';
    error.setAttribute('aria-live', 'polite');
    built.body.appendChild(error);

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
        var destination = status.destination_hash || '';
        addressValue.textContent = destination;
        address.classList.toggle('is-empty', !destination);
        addressLabel.textContent = destination ? 'Hub address' : 'Hub address appears after the first start';
        copyAddress.hidden = !destination;
        copyAddress.disabled = !destination;
        peopleMetric.value.textContent = String(Number(status.welcomed_sessions) || 0);
        roomMetric.value.textContent = String(Number(status.registered_rooms) || 0);
        registryWarning.hidden = !status.registry_degraded;
        renderDirty();
    }

    function applyResult(nextOverview, toastText) {
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
        saveConfig().then(function(nextOverview) {
            applyResult(nextOverview, 'Hub settings saved');
        }).catch(function(saveError) {
            error.textContent = (saveError && saveError.message) || 'Could not save hub settings.';
        }).then(function() {
            setBusy(false);
            renderStatus(overview);
        });
    });

    stateButton.addEventListener('click', function() {
        var action = stateButton.dataset.action;
        setBusy(true, action === 'start' ? 'Starting…' : 'Stopping…');
        error.textContent = '';
        var request = action === 'start'
            ? saveConfig().then(function() { return RS.invoke('channel_hub_start'); })
            : RS.invoke('channel_hub_stop');
        request.then(function(nextOverview) {
            applyResult(nextOverview, action === 'start' ? 'Your hub is ready' : 'Hub stopped');
        }).catch(function(actionError) {
            error.textContent = (actionError && actionError.message) ||
                (action === 'start' ? 'Could not start your hub.' : 'Could not stop your hub.');
            return channelHubLoad(true).then(function(nextOverview) {
                overview = nextOverview;
            }).catch(function() {});
        }).then(function() {
            setBusy(false);
            renderStatus(overview);
        });
    });

    _channelHubStatusRenderer = renderStatus;
    renderStatus(overview);
    _channelsPresentSheet(built, nameInput);
}

RS.listen('channel_hub_snapshot', function(status) {
    if (!channelHubOverview) return;
    channelHubOverview.status = status || {};
    _channelHubOverviewLoadedAt = Date.now();
    if (_channelHubStatusRenderer) _channelHubStatusRenderer(channelHubOverview);
});

RS.listen('lxmf_identity', function() {
    channelHubOverview = null;
    _channelHubOverviewLoadedAt = 0;
});
