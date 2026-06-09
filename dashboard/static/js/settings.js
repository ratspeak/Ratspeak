function openSettings() {
    switchView('settings');
    initSettingsSectionNav();
    showSettingsMobileSectionIndex({ restoreFocus: false });
    initAgentSettings();
    initHapticsToggle();
    initDeveloperModeToggle();
    syncSettingsIdentityActions();
    renderSettingsVersion();
    // Re-seal every System reset subsection on each visit. The collapse IS the
    // safety feature for destructive ops — a stale-open Delete Data section
    // from a previous visit would defeat it.
    resetSystemDataCollapse();
}

var _settingsVersionLabel = '';
var _settingsVersionValue = '';
var _settingsUpdateCheckInFlight = false;
var _settingsDeveloperModeBound = false;
var _settingsDeveloperModeStorageKey = 'ratspeak-developer-mode-enabled';
var _settingsDeveloperModeEnabled = readDeveloperModePreference();
var RATSPEAK_RELEASE_LATEST_URL = 'https://api.github.com/repos/ratspeak/Ratspeak/releases/latest';
var RATSPEAK_RELEASES_URL = 'https://github.com/ratspeak/Ratspeak/releases';

function readDeveloperModePreference() {
    try {
        return window.localStorage.getItem(_settingsDeveloperModeStorageKey) === 'true';
    } catch (e) {
        return false;
    }
}

function persistDeveloperModePreference(enabled) {
    try {
        window.localStorage.setItem(_settingsDeveloperModeStorageKey, enabled ? 'true' : 'false');
    } catch (e) {}
}

function notifyDeveloperModeChanged() {
    window.dispatchEvent(new CustomEvent('ratspeak-developer-mode-changed', {
        detail: { enabled: _settingsDeveloperModeEnabled }
    }));
}

function setDeveloperModeEnabled(enabled) {
    _settingsDeveloperModeEnabled = !!enabled;
    persistDeveloperModePreference(_settingsDeveloperModeEnabled);
    syncDeveloperModeRadioState();
    notifyDeveloperModeChanged();
}

window.ratspeakDeveloperModeEnabled = function() {
    return !!_settingsDeveloperModeEnabled;
};

function syncDeveloperModeRadioState() {
    var off = document.getElementById('settings-developer-mode-off');
    var on = document.getElementById('settings-developer-mode-on');
    if (off) off.checked = !_settingsDeveloperModeEnabled;
    if (on) on.checked = _settingsDeveloperModeEnabled;
}

function initDeveloperModeToggle() {
    var off = document.getElementById('settings-developer-mode-off');
    var on = document.getElementById('settings-developer-mode-on');
    if (!off || !on) return;
    syncDeveloperModeRadioState();
    if (_settingsDeveloperModeBound) return;
    _settingsDeveloperModeBound = true;

    off.addEventListener('click', function() {
        setDeveloperModeEnabled(false);
    });

    on.addEventListener('change', function() {
        if (on.checked) setDeveloperModeEnabled(true);
    });
}

function renderSettingsVersion() {
    var targets = [
        document.getElementById('settings-version-sidebar'),
        document.getElementById('settings-version-system')
    ].filter(Boolean);
    if (!targets.length) return;

    function paint(label, version) {
        targets.forEach(function(el) {
            el.innerHTML = '';
            if (label) {
                var versionLabel = document.createElement('span');
                versionLabel.className = 'settings-version-label';
                versionLabel.textContent = label;
                el.appendChild(versionLabel);

                var button = document.createElement('button');
                button.type = 'button';
                button.className = 'settings-update-check-btn';
                button.textContent = 'Check for updates';
                button.disabled = _settingsUpdateCheckInFlight;
                button.addEventListener('click', function() {
                    promptRatspeakUpdateCheck(version);
                });
                el.appendChild(button);
            }
            el.style.display = label ? '' : 'none';
        });
    }

    if (_settingsVersionLabel) {
        paint(_settingsVersionLabel, _settingsVersionValue);
        return;
    }

    RS.invoke('api_version').then(function(data) {
        var name = (data && data.name) || 'Ratspeak';
        var version = (data && data.version) || '';
        if (!version) return;
        _settingsVersionValue = version;
        _settingsVersionLabel = name + ' v.' + version;
        paint(_settingsVersionLabel, _settingsVersionValue);
    }).catch(function() {
        paint('', '');
    });
}

function _settingsNormalizeVersion(version) {
    return String(version || '')
        .trim()
        .replace(/^ratspeak\s+/i, '')
        .replace(/^v/i, '')
        .split(/[+\s]/)[0]
        .replace(/(\d)-([a-z]+)$/i, '$1$2');
}

function _settingsVersionParts(version) {
    return _settingsNormalizeVersion(version).split('.').map(function(part) {
        var match = String(part || '').match(/^(\d+)([a-z]*)/i);
        return {
            number: match ? parseInt(match[1], 10) : 0,
            suffix: match ? _settingsVersionSuffixRank(match[2]) : 0
        };
    });
}

function _settingsVersionSuffixRank(suffix) {
    suffix = String(suffix || '').toLowerCase();
    var rank = 0;
    for (var i = 0; i < suffix.length; i++) {
        var code = suffix.charCodeAt(i);
        if (code < 97 || code > 122) continue;
        rank = (rank * 27) + (code - 96);
    }
    return rank;
}

function _settingsCompareVersions(left, right) {
    var a = _settingsVersionParts(left);
    var b = _settingsVersionParts(right);
    var len = Math.max(a.length, b.length, 3);
    for (var i = 0; i < len; i++) {
        var av = a[i] || { number: 0, suffix: 0 };
        var bv = b[i] || { number: 0, suffix: 0 };
        if (av.number > bv.number) return 1;
        if (av.number < bv.number) return -1;
        if (av.suffix > bv.suffix) return 1;
        if (av.suffix < bv.suffix) return -1;
    }
    return 0;
}

function _settingsSetUpdateButtonsBusy(busy) {
    _settingsUpdateCheckInFlight = !!busy;
    document.querySelectorAll('.settings-update-check-btn').forEach(function(btn) {
        btn.disabled = _settingsUpdateCheckInFlight;
        btn.textContent = _settingsUpdateCheckInFlight ? 'Checking...' : 'Check for updates';
    });
}

function _settingsShowUpdateResult(title, message) {
    if (typeof rsAlert === 'function') {
        return rsAlert({ title: title, message: message, closeText: 'Close' });
    }
    showToast(title + ' ' + message, title === 'Update available!' ? 'toast-orange' : 'toast-blue', 6000);
    return Promise.resolve();
}

function _settingsLatestReleaseVersion(signal) {
    return fetch(RATSPEAK_RELEASE_LATEST_URL, {
        method: 'GET',
        cache: 'no-store',
        signal: signal,
        headers: {
            'Accept': 'application/vnd.github+json'
        }
    }).then(function(resp) {
        if (!resp.ok) throw new Error('GitHub returned ' + resp.status);
        return resp.json();
    }).then(function(data) {
        var latest = data && (data.tag_name || data.name);
        latest = _settingsNormalizeVersion(latest);
        if (!latest) throw new Error('No release version returned');
        return latest;
    });
}

function checkRatspeakUpdate(currentVersion) {
    currentVersion = _settingsNormalizeVersion(currentVersion || _settingsVersionValue);
    if (!currentVersion) {
        return _settingsShowUpdateResult(
            'Update check failed',
            'Could not determine the current Ratspeak version.'
        );
    }

    _settingsSetUpdateButtonsBusy(true);
    var progress = typeof rsProgress === 'function'
        ? rsProgress({ title: 'Checking for updates', message: 'Contacting GitHub...' })
        : null;
    var controller = typeof AbortController !== 'undefined' ? new AbortController() : null;
    var timeoutId = setTimeout(function() {
        if (controller) controller.abort();
    }, 12000);

    return _settingsLatestReleaseVersion(controller ? controller.signal : undefined).then(function(latestVersion) {
        if (progress) progress.close();
        if (_settingsCompareVersions(latestVersion, currentVersion) > 0) {
            return _settingsShowUpdateResult(
                'Update available!',
                "You're on version " + currentVersion + ', the latest version is ' + latestVersion + '. For privacy reasons, we do not currently auto-update. Please install the latest version for your device manually.'
            );
        }
        return _settingsShowUpdateResult(
            'Up to date!',
            "You're on the latest version of Ratspeak, no update available."
        );
    }).catch(function(err) {
        if (progress) progress.close();
        window.RS.diag('warn', '[settings] update check failed:', err);
        return _settingsShowUpdateResult(
            'Update check failed',
            'Could not check for updates. Confirm your internet connection and try again, or visit ' + RATSPEAK_RELEASES_URL + ' manually.'
        );
    }).finally(function() {
        clearTimeout(timeoutId);
        _settingsSetUpdateButtonsBusy(false);
    });
}

function promptRatspeakUpdateCheck(currentVersion) {
    if (_settingsUpdateCheckInFlight) return;
    rsConfirm({
        title: 'Check for updates?',
        message: 'Checking for the latest version requires an internet connection and will compare your version with the current available version. Are you sure?',
        confirmText: 'Yes',
        cancelText: 'No'
    }).then(function(ok) {
        if (!ok) return;
        return checkRatspeakUpdate(currentVersion);
    });
}

// Seal all System reset subsections. Called on every Settings open so the
// destructive/nuclear sections never start expanded after a previous session.
function resetSystemDataCollapse() {
    var sections = document.querySelectorAll('#panel-settings-system .system-subsection');
    for (var i = 0; i < sections.length; i++) {
        sections[i].classList.add('collapsed');
        var header = sections[i].querySelector('.system-subsection-header');
        if (header) header.setAttribute('aria-expanded', 'false');
    }
}

function toggleSystemSubsection(headerEl) {
    var section = headerEl.closest('.system-subsection');
    if (!section) return;
    section.classList.toggle('collapsed');
    var collapsed = section.classList.contains('collapsed');
    headerEl.setAttribute('aria-expanded', collapsed ? 'false' : 'true');
}

function handleSystemSubsectionKey(e) {
    if (e.key === 'Enter' || e.key === ' ' || e.key === 'Spacebar') {
        e.preventDefault();
        toggleSystemSubsection(e.currentTarget);
    }
}

var SETTINGS_DEFAULT_PANEL_ID = 'panel-settings-general';
var _settingsSectionNavBound = false;
var _settingsSectionResizeBound = false;
var _settingsMobileBackBound = false;
var _settingsMobileDetailActive = false;

function _settingsDetailModeActive() {
    return !!(window.matchMedia && window.matchMedia('(min-width: 769px)').matches);
}

function _settingsMobileModeActive() {
    return !!(window.matchMedia && window.matchMedia('(max-width: 768px)').matches);
}

function _settingsPanelAvailable(panel) {
    return !!(panel && !panel.hidden && panel.style.display !== 'none');
}

function _settingsFirstAvailablePanelId() {
    var items = document.querySelectorAll('.settings-nav-item[data-settings-panel]');
    for (var i = 0; i < items.length; i++) {
        var panelId = items[i].dataset.settingsPanel;
        if (_settingsPanelAvailable(document.getElementById(panelId))) return panelId;
    }
    return SETTINGS_DEFAULT_PANEL_ID;
}

function syncSettingsNavVisibility() {
    var activeHidden = false;
    document.querySelectorAll('.settings-nav-item[data-settings-panel]').forEach(function(item) {
        var panel = document.getElementById(item.dataset.settingsPanel);
        var available = _settingsPanelAvailable(panel);
        item.style.display = available ? '' : 'none';
        if (!available && item.classList.contains('active')) activeHidden = true;
    });
    if (activeHidden) {
        selectSettingsSection(_settingsFirstAvailablePanelId(), { skipStore: true });
    }
}

function selectSettingsSection(panelId, opts) {
    opts = opts || {};
    var panel = document.getElementById(panelId);
    if (!_settingsPanelAvailable(panel)) {
        panelId = _settingsFirstAvailablePanelId();
        panel = document.getElementById(panelId);
    }
    if (!panel) return;

    var detailMode = _settingsDetailModeActive();
    document.querySelectorAll('.settings-panel').forEach(function(el) {
        var selected = el.id === panelId;
        el.classList.toggle('settings-panel-selected', selected);
        if (detailMode) {
            el.setAttribute('aria-hidden', selected ? 'false' : 'true');
        } else {
            el.removeAttribute('aria-hidden');
        }
    });

    document.querySelectorAll('.settings-nav-item[data-settings-panel]').forEach(function(item) {
        var selected = item.dataset.settingsPanel === panelId;
        item.classList.toggle('active', selected);
        if (selected) item.setAttribute('aria-current', 'page');
        else item.removeAttribute('aria-current');
    });

    var activeItem = document.querySelector('.settings-nav-item[data-settings-panel="' + panelId + '"]');
    var title = document.getElementById('settings-detail-title');
    var desc = document.getElementById('settings-detail-desc');
    var mobileTitle = document.getElementById('settings-mobile-detail-title');
    if (activeItem) {
        var settingsTitle = activeItem.dataset.settingsTitle || activeItem.textContent.trim();
        if (title) title.textContent = settingsTitle;
        if (mobileTitle) mobileTitle.textContent = settingsTitle;
        if (desc) desc.textContent = activeItem.dataset.settingsDesc || '';
    }

    if (!opts.skipStore) {
        try { localStorage.setItem('ratspeak_settings_section', panelId); } catch(e) {}
    }

    if (opts.showMobileDetail) _settingsMobileDetailActive = true;
    syncSettingsMobileLayout();
}

function initSettingsSectionNav() {
    var nav = document.getElementById('settings-section-nav');
    if (!nav) return;

    if (!_settingsSectionNavBound) {
        nav.querySelectorAll('.settings-nav-item[data-settings-panel]').forEach(function(item) {
            item.addEventListener('click', function() {
                selectSettingsSection(item.dataset.settingsPanel, { showMobileDetail: _settingsMobileModeActive() });
            });
        });
        _settingsSectionNavBound = true;
    }

    if (!_settingsMobileBackBound) {
        var backBtn = document.getElementById('settings-mobile-back-btn');
        if (backBtn) {
            backBtn.addEventListener('click', showSettingsMobileSectionIndex);
            _settingsMobileBackBound = true;
        }
    }

    if (!_settingsSectionResizeBound) {
        window.addEventListener('resize', function() {
            var active = document.querySelector('.settings-nav-item.active[data-settings-panel]');
            selectSettingsSection(active ? active.dataset.settingsPanel : _settingsFirstAvailablePanelId(), { skipStore: true });
        });
        _settingsSectionResizeBound = true;
    }

    syncSettingsNavVisibility();
    var selected = SETTINGS_DEFAULT_PANEL_ID;
    try {
        selected = localStorage.getItem('ratspeak_settings_section') || selected;
    } catch(e) {}
    selectSettingsSection(selected, { skipStore: true });
    if (_settingsMobileModeActive()) _settingsMobileDetailActive = false;
    syncSettingsMobileLayout();
}

function syncSettingsMobileLayout() {
    var page = document.querySelector('#view-settings .settings-page');
    if (!page) return;
    var mobile = _settingsMobileModeActive();
    page.classList.toggle('settings-mobile-mode', mobile);
    page.classList.toggle('settings-mobile-detail-active', mobile && _settingsMobileDetailActive);
}

function isSettingsMobileDetailActive() {
    return _settingsMobileModeActive() && _settingsMobileDetailActive;
}

function showSettingsMobileSectionIndex(opts) {
    opts = opts || {};
    _settingsMobileDetailActive = false;
    syncSettingsMobileLayout();
    if (_settingsMobileModeActive()) {
        var view = document.getElementById('view-settings');
        if (view) view.scrollTop = 0;
        if (opts.restoreFocus !== false) {
            var activeItem = document.querySelector('.settings-nav-item.active[data-settings-panel]');
            if (activeItem) requestAnimationFrame(function() { activeItem.focus({ preventScroll: true }); });
        }
    }
}

var _settingsAgentsBound = false;
var _settingsAgentsLoading = false;
var _settingsAgentsState = {
    agents: [],
    presets: {},
    adapters: {},
    selected: null,
    detail: null,
    approvalState: 'pending_approval',
    approvals: [],
    audit: []
};

var AGENT_PRESET_ORDER = [
    'reply-assistant',
    'media-assistant',
    'inbox-reader',
    'network-helper',
    'openclaw-basic'
];

var AGENT_ADAPTER_ORDER = [
    'venice',
    'openrouter',
    'openclaw'
];

var AGENT_APPROVAL_STATES = [
    { label: 'Pending', value: 'pending_approval', hint: 'Needs owner review.' },
    { label: 'Approved', value: 'approved', hint: 'Ready for the agent daemon to execute.' },
    { label: 'Drafts', value: 'draft', hint: 'Created but not submitted.' },
    { label: 'Rejected', value: 'rejected', hint: 'Denied by the owner or policy.' },
    { label: 'Cancelled', value: 'cancelled', hint: 'Stopped before execution.' },
    { label: 'Expired', value: 'expired', hint: 'Timed out before approval or execution.' },
    { label: 'Sent', value: 'sent', hint: 'Message action completed.' },
    { label: 'Applied', value: 'applied', hint: 'Non-message action completed.' },
    { label: 'Failed', value: 'failed', hint: 'Execution failed.' }
];

function initAgentSettings() {
    var panel = document.getElementById('panel-settings-agents');
    if (!panel) return;

    if (!_settingsAgentsBound) {
        _settingsAgentsBound = true;

        var refreshBtn = document.getElementById('settings-refresh-agents-btn');
        if (refreshBtn) refreshBtn.addEventListener('click', function() { loadAgentSettings(true); });

        var createBtn = document.getElementById('settings-create-agent-btn');
        if (createBtn) createBtn.addEventListener('click', openAgentCreateFlow);

        var approvalRefresh = document.getElementById('settings-refresh-agent-approvals-btn');
        if (approvalRefresh) approvalRefresh.addEventListener('click', function() {
            loadAgentApprovals(_settingsAgentsState.selected);
        });

        var approvalStateBtn = document.getElementById('settings-agent-approval-state-btn');
        if (approvalStateBtn) approvalStateBtn.addEventListener('click', chooseAgentApprovalState);

        var expireBtn = document.getElementById('settings-expire-agent-actions-btn');
        if (expireBtn) expireBtn.addEventListener('click', expireAgentActions);

        var list = document.getElementById('settings-agents-list');
        if (list) list.addEventListener('click', handleAgentListClick);

        var detail = document.getElementById('settings-agent-detail');
        if (detail) detail.addEventListener('click', handleAgentDetailClick);
        if (detail) detail.addEventListener('change', handleAgentPolicyToggle);

        var approvals = document.getElementById('settings-agent-approvals-list');
        if (approvals) approvals.addEventListener('click', handleAgentApprovalClick);

        RS.listen('agents_updated', function() { loadAgentSettings(false); });
        RS.listen('agent_actions_updated', function() {
            loadAgentApprovals(_settingsAgentsState.selected);
            if (_settingsAgentsState.selected) loadAgentDetail(_settingsAgentsState.selected);
        });
    }

    loadAgentSettings(false);
}

function loadAgentSettings(force) {
    if (_settingsAgentsLoading && !force) return;
    _settingsAgentsLoading = true;
    var summary = document.getElementById('settings-agents-summary');
    var list = document.getElementById('settings-agents-list');
    if (summary) summary.textContent = 'Loading agent access settings...';
    if (list && !_settingsAgentsState.agents.length) {
        list.innerHTML = '<div class="inline-hint">Loading agents...</div>';
    }

    RS.invoke('api_agents').then(function(payload) {
        _settingsAgentsLoading = false;
        _settingsAgentsState.agents = (payload && payload.agents) || [];
        _settingsAgentsState.presets = (payload && payload.presets) || {};
        _settingsAgentsState.adapters = (payload && payload.adapters) || {};
        if (summary) {
            var count = _settingsAgentsState.agents.length;
            summary.textContent = count
                ? (count + ' agent' + (count === 1 ? '' : 's') + ' configured.')
                : 'Create an agent identity with scoped access to Ratspeak.';
        }
        if (!_settingsAgentsState.selected && _settingsAgentsState.agents.length) {
            _settingsAgentsState.selected = _settingsAgentsState.agents[0].name;
        }
        if (_settingsAgentsState.selected && !_settingsAgentsState.agents.some(function(a) { return a.name === _settingsAgentsState.selected; })) {
            _settingsAgentsState.selected = _settingsAgentsState.agents.length ? _settingsAgentsState.agents[0].name : null;
        }
        renderAgentList();
        if (_settingsAgentsState.selected) loadAgentDetail(_settingsAgentsState.selected);
        else renderAgentEmptyDetail();
        loadAgentApprovals(_settingsAgentsState.selected);
    }).catch(function(err) {
        _settingsAgentsLoading = false;
        if (summary) summary.textContent = 'Agent settings failed to load.';
        if (list) list.innerHTML = '<div class="inline-error">Failed to load agents.</div>';
        showAgentError(err, 'Failed to load agents');
    });
}

function renderAgentList() {
    var list = document.getElementById('settings-agents-list');
    if (!list) return;
    if (!_settingsAgentsState.agents.length) {
        list.innerHTML = '<div class="settings-agent-empty">' +
            '<span class="settings-agent-empty-title">No agents yet</span>' +
            '<span class="settings-agent-empty-copy">Add an agent to create its identity, scoped grant, local token file, and default guardrails.</span>' +
        '</div>';
        return;
    }

    list.innerHTML = _settingsAgentsState.agents.map(function(agent) {
        var active = agent.name === _settingsAgentsState.selected;
        var status = agent.status || 'active';
        var pending = agent.counts && agent.counts.pending_approval ? agent.counts.pending_approval : 0;
        var auto = agent.auto_approval_enabled ? 'auto allowed' : 'manual review';
        var adapter = agent.adapter && agent.adapter.configured ? agent.adapter.label : 'runtime not set';
        var runtime = agent.runtime && agent.runtime.running ? 'running' : (agent.runtime && agent.runtime.state ? agent.runtime.state : 'stopped');
        var health = agent.health && agent.health.ok === false ? ' · needs attention' : '';
        return '<button class="settings-agent-row' + (active ? ' active' : '') + '" type="button" data-agent="' + escapeHtml(agent.name) + '">' +
            '<span class="settings-agent-row-main">' +
                '<span class="settings-agent-row-name">' + escapeHtml(agent.display_name || agent.name) + '</span>' +
                '<span class="settings-agent-row-meta">' + escapeHtml(shortHash(agent.identity_hash || '', 8, 4)) + ' · ' + escapeHtml(adapter) + ' · ' + escapeHtml(auto) + health + '</span>' +
            '</span>' +
            '<span class="settings-agent-row-status status-' + escapeHtml(status) + '">' + escapeHtml(status) + '</span>' +
            '<span class="settings-agent-row-status status-runtime-' + escapeHtml(runtime) + '">' + escapeHtml(runtime) + '</span>' +
            (pending ? '<span class="settings-agent-row-count">' + pending + '</span>' : '') +
        '</button>';
    }).join('');
}

function renderAgentEmptyDetail() {
    var detail = document.getElementById('settings-agent-detail');
    var approvals = document.getElementById('settings-agent-approvals-list');
    var audit = document.getElementById('settings-agent-audit-list');
    if (detail) detail.innerHTML = '<div class="inline-hint">Select or add an agent to review permissions, approvals, and guardrails.</div>';
    if (approvals) approvals.innerHTML = '<div class="inline-hint">No pending approvals.</div>';
    if (audit) audit.innerHTML = '<div class="inline-hint">Agent audit entries appear here.</div>';
}

function loadAgentDetail(name) {
    if (!name) { renderAgentEmptyDetail(); return; }
    var detail = document.getElementById('settings-agent-detail');
    if (detail) detail.innerHTML = '<div class="inline-hint">Loading agent...</div>';
    RS.invoke('api_agent', { name: name }).then(function(payload) {
        _settingsAgentsState.selected = name;
        _settingsAgentsState.detail = payload;
        renderAgentList();
        renderAgentDetail(payload);
        loadAgentApprovals(name);
        loadAgentAudit(name);
    }).catch(function(err) {
        if (detail) detail.innerHTML = '<div class="inline-error">Failed to load selected agent.</div>';
        showAgentError(err, 'Failed to load agent');
    });
}

function renderAgentDetail(payload) {
    var detail = document.getElementById('settings-agent-detail');
    if (!detail) return;
    var agent = payload && payload.agent;
    var summary = payload && payload.summary;
    var policy = payload && payload.policy;
    if (!agent) {
        renderAgentEmptyDetail();
        return;
    }

    var grant = agent.grant || {};
    var scopes = grant.scopes || agent.effective_scopes || [];
    var contacts = grant.allowed_contacts || agent.allowed_contacts || [];
    var conversations = grant.allowed_conversations || agent.allowed_conversations || [];
    var adapter = (payload && payload.adapter) || (summary && summary.adapter) || {};
    var runtime = (payload && payload.runtime) || (summary && summary.runtime) || {};
    var health = payload && payload.health;
    var healthErrors = health && health.ok === false && Array.isArray(health.errors) ? health.errors : [];
    var tokenFile = agent.auth && agent.auth.token_file ? agent.auth.token_file : '';
    var endpoint = runtime && runtime.endpoint_file ? runtime.endpoint_file : (payload.connection && payload.connection.daemon ? payload.connection.daemon.endpoint_file : '');
    var autoEnabled = !!(policy && policy.auto_approval_enabled);
    var maxText = policy && policy.max_text_chars ? policy.max_text_chars : 4096;
    var maxFile = policy && policy.max_file_bytes ? policy.max_file_bytes : 0;
    var attachments = !!(policy && policy.allow_message_attachments);
    detail.innerHTML =
        '<div class="settings-agent-summary">' +
            '<div class="settings-agent-summary-head">' +
                '<div>' +
                    '<div class="settings-agent-title">' + escapeHtml(agent.display_name || agent.name) + '</div>' +
                    '<div class="settings-agent-subtitle">' + escapeHtml(agent.name) + ' · ' + escapeHtml(shortHash(agent.identity_hash || '', 8, 4)) + '</div>' +
                '</div>' +
                '<div class="settings-agent-state-stack">' +
                    '<span class="settings-agent-state">' + escapeHtml(grant.status || 'active') + '</span>' +
                    '<span class="settings-agent-state status-runtime-' + escapeHtml(runtime.state || 'stopped') + '">' + escapeHtml(runtime.running ? 'running' : (runtime.state || 'stopped')) + '</span>' +
                '</div>' +
            '</div>' +
            (healthErrors.length ? renderAgentHealth(healthErrors) : '') +
            renderAgentSetupChecklist(payload.setup || (summary && summary.setup)) +
            '<div class="settings-agent-primary-grid">' +
                agentPrimaryPanel('Provider', adapter.configured ? (adapter.label || adapter.provider || 'Configured') : 'Not configured', agentAdapterRuntimeCopy(adapter), [
                    ['configure-adapter', adapter.configured ? 'Edit' : 'Set up']
                ]) +
                agentPrimaryPanel('Connection', runtime.running ? 'Running' : 'Stopped', endpoint || 'Agent daemon is not running.', [
                    ['start-daemon', runtime.running ? 'Running' : 'Start'],
                    ['refresh-runtime', 'Refresh'],
                    ['copy-bundle', 'Copy kit']
                ]) +
                agentPrimaryPanel('Access', contacts.length ? (contacts.length + ' allowed contact' + (contacts.length === 1 ? '' : 's')) : 'No contacts allowed', conversations.length ? (conversations.length + ' conversation rules') : 'Limit the agent before it can reply.', [
                    ['add-contact', 'Allow contact']
                ]) +
                agentPrimaryPanel('Autonomy', autoEnabled ? 'Trusted replies' : 'Manual review', autoEnabled ? 'Routine replies can auto-approve inside guardrails.' : 'Every send waits for owner approval.', [
                    ['set-autonomy-manual', 'Manual'],
                    ['set-autonomy-routine', 'Trusted replies']
                ]) +
            '</div>' +
            '<div class="settings-agent-safety-strip">' +
                '<div><span>Message limit</span><strong>' + escapeHtml(String(maxText)) + ' chars</strong></div>' +
                '<div><span>Files</span><strong>' + escapeHtml(attachments ? formatAgentBytes(maxFile) : 'Off') + '</strong></div>' +
                '<div><span>Fallback</span><strong>' + escapeHtml(policy && policy.require_owner_approval ? 'Review' : 'Policy only') + '</strong></div>' +
                '<button class="selector-badge selector-badge-no-caret" data-agent-action="quick-limits">Edit limits</button>' +
            '</div>' +
            '<details class="settings-agent-technical">' +
                '<summary>Technical details</summary>' +
                '<div class="settings-agent-facts">' +
                    agentFact('Provider', adapter.provider || 'Not set') +
                    agentFact('Model', adapter.model || 'Not set') +
                    agentFact('Base URL', adapter.base_url || 'Not set') +
                    agentFact('Secret', agentAdapterSecretLabel(adapter)) +
                    agentFact('Command', adapter.command && adapter.command.length ? adapter.command.join(' ') : 'Not set') +
                    agentFact('Daemon endpoint', endpoint || 'Not running') +
                    agentFact('Token file', tokenFile) +
                    agentFact('Scopes', scopes.length ? scopes.join(', ') : 'None') +
                    agentFact('Allowed contacts', contacts.length ? contacts.join(', ') : (grant.unknown_contacts === 'allow' ? 'All contacts' : 'None')) +
                    agentFact('Allowed conversations', conversations.length ? conversations.join(', ') : 'None') +
                '</div>' +
            '</details>' +
        '</div>' +
        renderAgentAdvancedGuardrails(policy || {}, summary);
}

function agentFact(label, value) {
    return '<div class="settings-agent-fact">' +
        '<span class="settings-agent-fact-label">' + escapeHtml(label) + '</span>' +
        '<span class="settings-agent-fact-value">' + escapeHtml(value || 'Not set') + '</span>' +
    '</div>';
}

function agentPrimaryPanel(title, value, detail, actions) {
    return '<section class="settings-agent-primary-panel">' +
        '<div class="settings-agent-primary-copy">' +
            '<span class="settings-agent-fact-label">' + escapeHtml(title) + '</span>' +
            '<strong>' + escapeHtml(value || 'Not set') + '</strong>' +
            '<span>' + escapeHtml(detail || '') + '</span>' +
        '</div>' +
        '<div class="settings-agent-primary-actions">' +
            (actions || []).map(function(action) {
                return '<button class="selector-badge selector-badge-no-caret" data-agent-action="' + escapeHtml(action[0]) + '">' + escapeHtml(action[1]) + '</button>';
            }).join('') +
        '</div>' +
    '</section>';
}

function agentAdapterRuntimeCopy(adapter) {
    if (!adapter || !adapter.configured) return 'Choose Venice, OpenRouter, or OpenClaw.';
    if (adapter.provider === 'openclaw') return 'Local OpenClaw adapter uses the copied connection kit.';
    var parts = [];
    if (adapter.model) parts.push(adapter.model);
    if (adapter.secret && adapter.secret.env) parts.push(adapter.secret.env);
    return parts.length ? parts.join(' via ') : 'API adapter configured.';
}

function renderAgentHealth(errors) {
    return '<div class="settings-agent-health">' +
        '<span class="settings-agent-health-title">Needs attention</span>' +
        errors.slice(0, 3).map(function(error) {
            return '<span class="settings-agent-health-copy">' + escapeHtml((error.area || 'agent') + ': ' + (error.message || error.code || 'unavailable')) + '</span>';
        }).join('') +
    '</div>';
}

function renderAgentSetupChecklist(setup) {
    var items = setup && Array.isArray(setup.items) ? setup.items : [];
    if (!items.length) return '';
    return '<div class="settings-agent-checklist">' + items.map(function(item) {
        return '<div class="settings-agent-checkitem' + (item.complete ? ' complete' : '') + '">' +
            '<span class="settings-agent-checkmark">' + (item.complete ? 'OK' : '--') + '</span>' +
            '<span class="settings-agent-checkbody">' +
                '<span class="settings-agent-checklabel">' + escapeHtml(item.label || item.key || 'Step') + '</span>' +
                '<span class="settings-agent-checkdetail">' + escapeHtml(item.detail || '') + '</span>' +
            '</span>' +
        '</div>';
    }).join('') + '</div>';
}

function agentAdapterSecretLabel(adapter) {
    var secret = adapter && adapter.secret ? adapter.secret : {};
    if (secret.env) return 'Environment: ' + secret.env;
    if (secret.file) return 'File: ' + secret.file;
    return 'External or none';
}

function renderAgentAdvancedGuardrails(policy, summary) {
    return '<details class="settings-agent-advanced">' +
        '<summary>Advanced guardrails</summary>' +
        '<div class="settings-agent-advanced-copy">Most users should use the simple autonomy and limit controls above. These options exist for operators who need exact policy tuning.</div>' +
        renderAgentPolicyControls(policy, summary) +
    '</details>';
}

function renderAgentPolicyControls(policy, summary) {
    var groups = [
        {
            title: 'Autonomy',
            controls: [
                { type: 'toggle', key: 'require_owner_approval', label: 'Manual review for unmatched actions', desc: 'When on, actions that do not match auto-approval still wait for owner review. Keep this on for first-run setups.', dangerOff: true },
                { type: 'toggle', key: 'auto_approval_enabled', label: 'Auto approval', desc: 'Allow low-risk matching actions to skip the approval queue.' },
                { type: 'list', key: 'auto_approval_allowed_action_kinds', label: 'Auto-approved action kinds', desc: 'Only these action kinds can auto-approve.' },
                { type: 'list', key: 'auto_approval_allowed_contacts', label: 'Auto-approved contacts', desc: 'Optional contact allowlist for auto-approved actions.' },
                { type: 'list', key: 'auto_approval_allowed_conversations', label: 'Auto-approved conversations', desc: 'Optional conversation allowlist for auto-approved actions.' },
                { type: 'list', key: 'auto_approval_allowed_delivery_methods', label: 'Auto delivery methods', desc: 'Delivery methods allowed for auto-approved sends.' },
                { type: 'choice', key: 'auto_approval_unknown_contacts', label: 'Auto unknown contacts', desc: 'How auto-approval treats contacts outside the allowlist.', choices: ['deny', 'allow'] },
                { type: 'toggle', key: 'auto_approval_requires_causal_context', label: 'Auto causal context', desc: 'Auto-approved replies must point back to an inbound event or message.' },
                { type: 'toggle', key: 'auto_approval_requires_verified_causal_context', label: 'Verified auto context', desc: 'Require the referenced event/message to match the conversation being answered.' },
                { type: 'toggle', key: 'auto_approval_allow_attachments', label: 'Auto attachments', desc: 'Allow attachments inside the auto-approval lane.' },
                { type: 'number', key: 'auto_approval_max_text_chars', label: 'Auto text characters', desc: 'Maximum characters for auto-approved text.' },
                { type: 'bytes', key: 'auto_approval_max_text_bytes', label: 'Auto text bytes', desc: 'Maximum UTF-8 bytes for auto-approved text.' },
                { type: 'bytes', key: 'auto_approval_max_attachment_bytes', label: 'Auto attachment bytes', desc: 'Maximum attachment bytes for auto-approved actions.' },
                { type: 'number', key: 'auto_approval_max_actions_per_hour', label: 'Auto actions per hour', desc: 'Hourly cap for auto-approved actions.' },
                { type: 'number', key: 'auto_approval_max_actions_per_day', label: 'Auto actions per day', desc: 'Daily cap for auto-approved actions.' },
                { type: 'number', key: 'auto_approval_max_messages_per_contact_hour', label: 'Auto messages/contact hour', desc: 'Per-contact hourly cap inside the auto-approval lane.' },
                { type: 'number', key: 'auto_approval_max_messages_per_contact_day', label: 'Auto messages/contact day', desc: 'Per-contact daily cap inside the auto-approval lane.' }
            ]
        },
        {
            title: 'Loop Prevention',
            controls: [
                { type: 'number', key: 'max_pending_actions', label: 'Maximum pending actions', desc: 'Backpressure cap for unreviewed agent work.' },
                { type: 'number', key: 'max_actions_per_hour', label: 'Actions per hour', desc: 'Global hourly action cap.' },
                { type: 'number', key: 'max_actions_per_day', label: 'Actions per day', desc: 'Global daily action cap.' },
                { type: 'number', key: 'max_messages_per_contact_hour', label: 'Messages/contact hour', desc: 'Per-contact hourly message cap.' },
                { type: 'number', key: 'max_messages_per_contact_day', label: 'Messages/contact day', desc: 'Per-contact daily message cap.' },
                { type: 'number', key: 'per_contact_cooldown_secs', label: 'Contact cooldown seconds', desc: 'Minimum delay between sends to the same contact.' },
                { type: 'number', key: 'inbound_loop_window_secs', label: 'Loop window seconds', desc: 'Window for detecting inbound-to-outbound feedback loops.' },
                { type: 'number', key: 'max_outbound_per_contact_window', label: 'Loop window outbound cap', desc: 'Maximum outbound messages per contact during the loop window.' },
                { type: 'toggle', key: 'require_causal_context_for_outbound', label: 'Require causal context', desc: 'Every outbound action must identify the event or message that caused it.' },
                { type: 'toggle', key: 'require_verified_causal_context', label: 'Verify causal context', desc: 'Causal references must exist and pass policy validation.' },
                { type: 'number', key: 'max_causal_age_secs', label: 'Maximum causal age seconds', desc: 'Reject actions tied to stale inbound context.' },
                { type: 'toggle', key: 'causal_subject_must_match', label: 'Causal subject must match', desc: 'Replies must stay bound to the same contact or conversation.' },
                { type: 'toggle', key: 'causal_event_must_be_inbound', label: 'Causal event must be inbound', desc: 'Outbound chains cannot self-trigger from outbound events.' },
                { type: 'number', key: 'max_actions_per_causal_event', label: 'Actions per causal event', desc: 'Maximum actions a single event can cause.' },
                { type: 'number', key: 'max_actions_per_causal_message', label: 'Actions per causal message', desc: 'Maximum actions a single message can cause.' }
            ]
        },
        {
            title: 'Messages & Content',
            controls: [
                { type: 'number', key: 'max_text_chars', label: 'Message text characters', desc: 'Hard character cap for agent-authored text.' },
                { type: 'bytes', key: 'max_text_bytes', label: 'Message text bytes', desc: 'Hard byte cap for agent-authored text.' },
                { type: 'number', key: 'max_title_chars', label: 'Title characters', desc: 'Hard character cap for titles or subjects.' },
                { type: 'bytes', key: 'max_title_bytes', label: 'Title bytes', desc: 'Hard byte cap for titles or subjects.' },
                { type: 'list', key: 'denied_text_substrings', label: 'Denied text fragments', desc: 'Case-sensitive text fragments that block staging.' },
                { type: 'toggle', key: 'reject_control_chars', label: 'Reject control characters', desc: 'Block unexpected control characters in text payloads.' },
                { type: 'toggle', key: 'allow_message_reactions', label: 'Reactions', desc: 'Allow message reaction actions.' },
                { type: 'number', key: 'max_reactions_per_hour', label: 'Reactions per hour', desc: 'Hourly cap for reactions.' },
                { type: 'number', key: 'max_reactions_per_day', label: 'Reactions per day', desc: 'Daily cap for reactions.' },
                { type: 'number', key: 'max_reactions_per_message', label: 'Reactions per message', desc: 'Per-message reaction cap.' },
                { type: 'toggle', key: 'reply_requires_existing_message', label: 'Reply needs existing message', desc: 'Replies must target a known message.' },
                { type: 'toggle', key: 'reply_to_must_match_causal_message', label: 'Reply target must match cause', desc: 'Reply-to targets must match the causal message.' }
            ]
        },
        {
            title: 'Files & Images',
            controls: [
                { type: 'toggle', key: 'allow_message_attachments', label: 'Attachments', desc: 'Allow file attachments.' },
                { type: 'toggle', key: 'allow_message_images', label: 'Images', desc: 'Allow image attachments.' },
                { type: 'toggle', key: 'require_owner_approval_for_attachments', label: 'Attachment approval', desc: 'Require owner review before attachment sends.' },
                { type: 'bytes', key: 'max_attachment_bytes', label: 'Attachment bytes/action', desc: 'Hard total attachment cap per action.' },
                { type: 'bytes', key: 'max_file_bytes', label: 'File size', desc: 'Hard cap for staged files.' },
                { type: 'bytes', key: 'max_image_bytes', label: 'Image size', desc: 'Hard cap for staged images.' },
                { type: 'number', key: 'max_attachments_per_action', label: 'Attachments per action', desc: 'Maximum number of staged attachments.' },
                { type: 'bytes', key: 'max_attachment_name_bytes', label: 'Attachment name bytes', desc: 'Maximum file name length in bytes.' },
                { type: 'toggle', key: 'allow_agent_file_paths', label: 'Agent file paths', desc: 'Allow the agent to stage local files by path.' },
                { type: 'list', key: 'allowed_source_roots', label: 'Allowed source roots', desc: 'Optional local directories allowed for path-based staging.' },
                { type: 'list', key: 'allowed_attachment_mime_prefixes', label: 'Allowed MIME prefixes', desc: 'MIME prefixes accepted for attachments.' },
                { type: 'list', key: 'denied_attachment_mime_prefixes', label: 'Denied MIME prefixes', desc: 'MIME prefixes always blocked.' }
            ]
        },
        {
            title: 'Contacts & Conversations',
            controls: [
                { type: 'toggle', key: 'allow_contact_mutations', label: 'Contact changes', desc: 'Allow contact add/edit/remove actions.' },
                { type: 'toggle', key: 'require_owner_approval_for_contact_mutations', label: 'Contact approval', desc: 'Require owner review for contact changes.' },
                { type: 'number', key: 'max_contact_mutations_per_hour', label: 'Contact changes/hour', desc: 'Hourly cap for contact changes.' },
                { type: 'number', key: 'max_contact_mutations_per_day', label: 'Contact changes/day', desc: 'Daily cap for contact changes.' },
                { type: 'toggle', key: 'allow_conversation_mutations', label: 'Conversation changes', desc: 'Allow conversation metadata changes.' },
                { type: 'toggle', key: 'allow_conversation_delete', label: 'Conversation delete', desc: 'Allow conversation deletion actions.' },
                { type: 'toggle', key: 'require_owner_approval_for_conversation_mutations', label: 'Conversation approval', desc: 'Require owner review for conversation changes.' },
                { type: 'number', key: 'max_conversation_mutations_per_hour', label: 'Conversation changes/hour', desc: 'Hourly cap for conversation changes.' },
                { type: 'number', key: 'max_conversation_mutations_per_day', label: 'Conversation changes/day', desc: 'Daily cap for conversation changes.' }
            ]
        },
        {
            title: 'Network & Propagation',
            controls: [
                { type: 'toggle', key: 'allow_identity_announce', label: 'Identity announces', desc: 'Allow agent-triggered announces.' },
                { type: 'toggle', key: 'allow_path_request', label: 'Path requests', desc: 'Allow agent-triggered path requests.' },
                { type: 'toggle', key: 'require_owner_approval_for_network', label: 'Network approval', desc: 'Require owner review for network-facing actions.' },
                { type: 'number', key: 'max_network_actions_per_hour', label: 'Network actions/hour', desc: 'Hourly cap for network actions.' },
                { type: 'number', key: 'max_network_actions_per_day', label: 'Network actions/day', desc: 'Daily cap for network actions.' },
                { type: 'number', key: 'max_announces_per_hour', label: 'Announces per hour', desc: 'Hourly cap for announces.' },
                { type: 'number', key: 'max_announces_per_day', label: 'Announces per day', desc: 'Daily cap for announces.' },
                { type: 'number', key: 'min_announce_interval_secs', label: 'Announce interval seconds', desc: 'Minimum delay between announces.' },
                { type: 'number', key: 'max_path_requests_per_hour', label: 'Path requests per hour', desc: 'Hourly cap for path requests.' },
                { type: 'number', key: 'max_path_requests_per_day', label: 'Path requests per day', desc: 'Daily cap for path requests.' },
                { type: 'number', key: 'min_path_request_interval_secs', label: 'Path request interval seconds', desc: 'Minimum delay between path requests.' },
                { type: 'toggle', key: 'allow_unknown_path_requests', label: 'Unknown path requests', desc: 'Allow path requests outside the explicit allowlist.' },
                { type: 'list', key: 'allowed_path_request_hashes', label: 'Path request hashes', desc: 'Optional path-request destination allowlist.' },
                { type: 'toggle', key: 'allow_forced_propagated_delivery', label: 'Propagated delivery', desc: 'Allow agent actions to force Offline Inbox delivery.' },
                { type: 'toggle', key: 'allow_static_propagation_nodes_only', label: 'Static propagation nodes only', desc: 'Restrict forced propagation to configured static nodes.' },
                { type: 'list', key: 'allowed_propagation_node_hashes', label: 'Propagation nodes', desc: 'Optional Offline Inbox node allowlist.' }
            ]
        },
        {
            title: 'Execution Boundaries',
            controls: [
                { type: 'toggle', key: 'deny_execute_on_policy_revision_change', label: 'Recheck policy revisions', desc: 'Block stale approved actions after policy changes.' },
                { type: 'toggle', key: 'deny_execute_on_grant_revision_change', label: 'Recheck grant revisions', desc: 'Block stale approved actions after grant changes.' },
                { type: 'list', key: 'blocked_action_kinds', label: 'Blocked action kinds', desc: 'Action kinds denied before approval checks.' },
                { type: 'list', key: 'allowed_delivery_methods', label: 'Delivery methods', desc: 'Delivery methods agents may request.' },
                { type: 'number', key: 'default_expires_secs', label: 'Default expiry seconds', desc: 'Default lifetime for new actions.' },
                { type: 'number', key: 'max_expires_secs', label: 'Maximum expiry seconds', desc: 'Maximum lifetime an agent can request.' }
            ]
        }
    ];
    return '<div class="settings-agent-policy-list">' + groups.map(function(group) {
        return '<div class="settings-agent-policy-group">' +
            '<div class="settings-panel-section-title">' + escapeHtml(group.title) + '</div>' +
            group.controls.map(function(spec) { return policyControl(spec, policy); }).join('') +
        '</div>';
    }).join('') + '</div>';
}

function policyControl(spec, policy) {
    var value = policy ? policy[spec.key] : undefined;
    if (spec.type === 'toggle') return policyToggle(spec.key, spec.label, spec.desc, !!value, !!spec.dangerOff);
    if (spec.type === 'bytes') return policyBytes(spec.key, spec.label, spec.desc, value || 0);
    if (spec.type === 'list') return policyList(spec.key, spec.label, spec.desc, Array.isArray(value) ? value : []);
    if (spec.type === 'choice') return policyChoice(spec.key, spec.label, spec.desc, value, spec.choices || []);
    return policyNumber(spec.key, spec.label, spec.desc, value);
}

function policyToggle(key, label, desc, checked) {
    var dangerOff = arguments.length > 4 && arguments[4];
    return '<div class="settings-row settings-agent-policy-row" data-policy-key="' + escapeHtml(key) + '">' +
        '<div class="settings-row-info"><span class="settings-row-label">' + escapeHtml(label) + '</span><span class="settings-row-desc">' + escapeHtml(desc) + '</span></div>' +
        '<label class="prop-toggle" aria-label="' + escapeHtml(label) + '">' +
            '<input type="checkbox" data-agent-policy-toggle="' + escapeHtml(key) + '"' + (dangerOff ? ' data-danger-off="1"' : '') + (checked ? ' checked' : '') + '>' +
            '<span class="prop-slider"></span>' +
        '</label>' +
    '</div>';
}

function policyNumber(key, label, desc, value) {
    return '<div class="settings-row settings-agent-policy-row" data-policy-key="' + escapeHtml(key) + '">' +
        '<div class="settings-row-info"><span class="settings-row-label">' + escapeHtml(label) + '</span><span class="settings-row-desc">' + escapeHtml(desc) + '</span></div>' +
        '<button class="selector-badge" data-agent-policy-number="' + escapeHtml(key) + '">' + escapeHtml(String(value == null ? 0 : value)) + '</button>' +
    '</div>';
}

function policyBytes(key, label, desc, value) {
    return '<div class="settings-row settings-agent-policy-row" data-policy-key="' + escapeHtml(key) + '">' +
        '<div class="settings-row-info"><span class="settings-row-label">' + escapeHtml(label) + '</span><span class="settings-row-desc">' + escapeHtml(desc) + '</span></div>' +
        '<button class="selector-badge" data-agent-policy-bytes="' + escapeHtml(key) + '">' + escapeHtml(formatAgentBytes(value || 0)) + '</button>' +
    '</div>';
}

function policyList(key, label, desc, values) {
    var count = values.length ? values.length + ' set' : 'Any';
    return '<div class="settings-row settings-agent-policy-row" data-policy-key="' + escapeHtml(key) + '">' +
        '<div class="settings-row-info"><span class="settings-row-label">' + escapeHtml(label) + '</span><span class="settings-row-desc">' + escapeHtml(desc) + '</span></div>' +
        '<button class="selector-badge" data-agent-policy-list="' + escapeHtml(key) + '">' + escapeHtml(count) + '</button>' +
    '</div>';
}

function policyChoice(key, label, desc, value, choices) {
    return '<div class="settings-row settings-agent-policy-row" data-policy-key="' + escapeHtml(key) + '" data-policy-choices="' + escapeHtml(choices.join(',')) + '">' +
        '<div class="settings-row-info"><span class="settings-row-label">' + escapeHtml(label) + '</span><span class="settings-row-desc">' + escapeHtml(desc) + '</span></div>' +
        '<button class="selector-badge" data-agent-policy-choice="' + escapeHtml(key) + '">' + escapeHtml(String(value || '')) + '</button>' +
    '</div>';
}

function handleAgentListClick(e) {
    var row = e.target.closest('.settings-agent-row[data-agent]');
    if (!row) return;
    _settingsAgentsState.selected = row.dataset.agent;
    renderAgentList();
    loadAgentDetail(row.dataset.agent);
}

function handleAgentDetailClick(e) {
    var numberBtn = e.target.closest('[data-agent-policy-number]');
    if (numberBtn) {
        editAgentPolicyNumber(numberBtn.dataset.agentPolicyNumber, numberBtn.textContent.trim());
        return;
    }
    var bytesBtn = e.target.closest('[data-agent-policy-bytes]');
    if (bytesBtn) {
        editAgentPolicyBytes(bytesBtn.dataset.agentPolicyBytes);
        return;
    }
    var listBtn = e.target.closest('[data-agent-policy-list]');
    if (listBtn) {
        editAgentPolicyList(listBtn.dataset.agentPolicyList);
        return;
    }
    var choiceBtn = e.target.closest('[data-agent-policy-choice]');
    if (choiceBtn) {
        editAgentPolicyChoice(choiceBtn.dataset.agentPolicyChoice);
        return;
    }
    var actionBtn = e.target.closest('[data-agent-action]');
    if (!actionBtn) return;
    var action = actionBtn.dataset.agentAction;
    if (action === 'configure-adapter') configureSelectedAgentAdapter();
    else if (action === 'start-daemon') startSelectedAgentDaemon();
    else if (action === 'refresh-runtime') refreshSelectedAgentRuntime();
    else if (action === 'copy-bundle') copyAgentConnectionBundle();
    else if (action === 'add-contact') addAgentAllowedContact();
    else if (action === 'edit-scopes') editAgentPreset();
    else if (action === 'set-autonomy-manual') setAgentAutonomyManual();
    else if (action === 'set-autonomy-routine') setAgentAutonomyRoutine();
    else if (action === 'quick-limits') editAgentQuickLimits();
    else if (action === 'rotate-token') rotateSelectedAgentToken();
    else if (action === 'revoke') revokeSelectedAgent();
}

function handleAgentPolicyToggle(e) {
    var input = e.target.closest('[data-agent-policy-toggle]');
    if (!input) return;
    if (input.dataset.dangerOff === '1' && !input.checked) {
        input.checked = true;
        rsConfirm({
            title: 'Disable Manual Review',
            message: 'Actions that do not match auto-approval may run without owner review. Keep this on unless you have a controlled agent and tight grants.',
            danger: true,
            confirmText: 'Disable'
        }).then(function(ok) {
            if (!ok) return;
            input.checked = false;
            setSelectedAgentPolicy(input.dataset.agentPolicyToggle, false);
        });
        return;
    }
    setSelectedAgentPolicy(input.dataset.agentPolicyToggle, !!input.checked);
}

function selectedAgentName() {
    return _settingsAgentsState.selected;
}

function setSelectedAgentPolicy(key, value) {
    var name = selectedAgentName();
    if (!name) return Promise.resolve(null);
    return RS.invoke('set_agent_policy', {
        args: {
            name: name,
            set: [{ key: key, value: value }]
        }
    }).then(function(payload) {
        if (payload && payload.policy) {
            if (!_settingsAgentsState.detail) _settingsAgentsState.detail = {};
            _settingsAgentsState.detail.policy = payload.policy;
            renderAgentDetail(_settingsAgentsState.detail);
        }
        showToast('Agent policy updated', 'toast-green', 1800);
        loadAgentSettings(false);
        return payload;
    }).catch(function(err) {
        showAgentError(err, 'Failed to update policy');
        loadAgentDetail(name);
    });
}

function setSelectedAgentPolicyBatch(sets, toastLabel) {
    var name = selectedAgentName();
    if (!name) return Promise.resolve(null);
    return RS.invoke('set_agent_policy', {
        args: {
            name: name,
            set: sets
        }
    }).then(function(payload) {
        if (payload && payload.policy) {
            if (!_settingsAgentsState.detail) _settingsAgentsState.detail = {};
            _settingsAgentsState.detail.policy = payload.policy;
            renderAgentDetail(_settingsAgentsState.detail);
        }
        showToast(toastLabel || 'Agent policy updated', 'toast-green', 1800);
        loadAgentSettings(false);
        return payload;
    }).catch(function(err) {
        showAgentError(err, 'Failed to update policy');
        loadAgentDetail(name);
    });
}

function setAgentAutonomyManual() {
    setSelectedAgentPolicyBatch([
        { key: 'require_owner_approval', value: true },
        { key: 'auto_approval_enabled', value: false }
    ], 'Manual review enabled');
}

function setAgentAutonomyRoutine() {
    rsConfirm({
        title: 'Trust Routine Replies',
        message: 'Routine text replies to allowed contacts can skip approval when they fit message, rate, and causal-context guardrails. Anything outside that lane still waits for review.',
        confirmText: 'Trust replies'
    }).then(function(ok) {
        if (!ok) return null;
        return setSelectedAgentPolicyBatch([
            { key: 'require_owner_approval', value: true },
            { key: 'auto_approval_enabled', value: true },
            { key: 'auto_approval_allowed_action_kinds', value: ['message.reply', 'message.send'] },
            { key: 'auto_approval_requires_causal_context', value: true },
            { key: 'auto_approval_requires_verified_causal_context', value: true },
            { key: 'auto_approval_allow_attachments', value: false },
            { key: 'auto_approval_max_text_chars', value: 1500 },
            { key: 'auto_approval_max_actions_per_hour', value: 20 },
            { key: 'auto_approval_max_messages_per_contact_hour', value: 10 }
        ], 'Trusted replies enabled');
    });
}

function editAgentQuickLimits() {
    var detail = _settingsAgentsState.detail || {};
    var policy = detail.policy || {};
    rsChoice({
        title: 'Agent Limits',
        message: 'Choose the limit to change.',
        choices: [
            { label: 'Message length', value: 'max_text_chars', hint: (policy.max_text_chars || 4096) + ' chars' },
            { label: 'File size', value: 'max_file_bytes', hint: formatAgentBytes(policy.max_file_bytes || 0) },
            { label: 'Image size', value: 'max_image_bytes', hint: formatAgentBytes(policy.max_image_bytes || 0) },
            { label: 'Attachments', value: 'allow_message_attachments', hint: policy.allow_message_attachments ? 'On' : 'Off' }
        ]
    }).then(function(choice) {
        if (!choice) return null;
        if (choice === 'allow_message_attachments') {
            return setSelectedAgentPolicy(choice, !policy.allow_message_attachments);
        }
        if (choice === 'max_text_chars') {
            return editAgentPolicyNumber(choice, String(policy.max_text_chars || 4096));
        }
        return editAgentPolicyBytes(choice);
    });
}

function editAgentPolicyNumber(key, currentText) {
    var detail = _settingsAgentsState.detail || {};
    var policy = detail.policy || {};
    rsPrompt({
        title: 'Agent Guardrail',
        message: key.replace(/_/g, ' '),
        placeholder: 'Number',
        defaultValue: String(policy[key] == null ? parseInt(currentText, 10) || 0 : policy[key]),
        confirmText: 'Save'
    }).then(function(input) {
        if (input === null) return;
        var value = parseInt(input, 10);
        if (!isFinite(value) || value < 0) value = 0;
        setSelectedAgentPolicy(key, value);
    });
}

function editAgentPolicyBytes(key) {
    var detail = _settingsAgentsState.detail || {};
    var policy = detail.policy || {};
    var current = policy[key] || 0;
    rsChoice({
        title: 'Size Guardrail',
        message: key.replace(/_/g, ' '),
        choices: [
            { label: '64 KiB', value: String(64 * 1024) },
            { label: '256 KiB', value: String(256 * 1024) },
            { label: '1 MiB', value: String(1024 * 1024) },
            { label: 'Current: ' + formatAgentBytes(current), value: String(current) },
            { label: 'Custom...', value: 'custom' }
        ]
    }).then(function(value) {
        if (value === null) return null;
        if (value !== 'custom') return value;
        return rsPrompt({
            title: 'Custom Size',
            message: 'Enter bytes.',
            placeholder: '262144',
            defaultValue: String(current),
            confirmText: 'Save'
        });
    }).then(function(value) {
        if (value === null || value === undefined) return;
        var bytes = parseInt(value, 10);
        if (!isFinite(bytes) || bytes < 0) bytes = 0;
        setSelectedAgentPolicy(key, bytes);
    });
}

function editAgentPolicyList(key) {
    var detail = _settingsAgentsState.detail || {};
    var policy = detail.policy || {};
    var current = (policy[key] || []).map(function(value) { return String(value); }).join(', ');
    var copy = agentPolicyListCopy(key);
    rsPrompt({
        title: copy.title,
        message: copy.message,
        placeholder: copy.placeholder,
        defaultValue: current,
        confirmText: 'Save'
    }).then(function(input) {
        if (input === null) return;
        var values = input.split(',').map(function(v) { return v.trim(); }).filter(Boolean);
        setSelectedAgentPolicy(key, values);
    });
}

function agentPolicyListCopy(key) {
    var defaults = {
        title: 'Agent Values',
        message: 'Comma-separated values. Leave blank for none.',
        placeholder: 'value1, value2'
    };
    var map = {
        auto_approval_allowed_action_kinds: {
            title: 'Auto Action Kinds',
            message: 'Comma-separated action kinds allowed to auto-approve.',
            placeholder: 'message.send, message.reply'
        },
        auto_approval_allowed_contacts: {
            title: 'Auto Contacts',
            message: 'Comma-separated contact hashes allowed for auto-approval.',
            placeholder: '32 hex characters'
        },
        auto_approval_allowed_conversations: {
            title: 'Auto Conversations',
            message: 'Comma-separated conversation IDs allowed for auto-approval.',
            placeholder: 'lxmf:32hex'
        },
        auto_approval_allowed_delivery_methods: {
            title: 'Auto Delivery Methods',
            message: 'Comma-separated delivery methods allowed for auto-approved sends.',
            placeholder: 'auto, direct'
        },
        denied_text_substrings: {
            title: 'Denied Text',
            message: 'Comma-separated text fragments that block staging.',
            placeholder: 'fragment1, fragment2'
        },
        allowed_source_roots: {
            title: 'Source Roots',
            message: 'Comma-separated local folders allowed for path-based file staging.',
            placeholder: '/Users/name/Documents'
        },
        allowed_attachment_mime_prefixes: {
            title: 'Allowed MIME Prefixes',
            message: 'Comma-separated MIME prefixes accepted for attachments.',
            placeholder: 'text/, image/'
        },
        denied_attachment_mime_prefixes: {
            title: 'Denied MIME Prefixes',
            message: 'Comma-separated MIME prefixes always blocked.',
            placeholder: 'application/x-'
        },
        allowed_path_request_hashes: {
            title: 'Path Request Hashes',
            message: 'Comma-separated destination hashes the agent may request paths for.',
            placeholder: '32 hex characters'
        },
        allowed_propagation_node_hashes: {
            title: 'Propagation Nodes',
            message: 'Comma-separated Offline Inbox node hashes allowed for forced propagation.',
            placeholder: '32 hex characters'
        },
        blocked_action_kinds: {
            title: 'Blocked Action Kinds',
            message: 'Comma-separated action kinds denied before approval checks.',
            placeholder: 'identity.announce'
        },
        allowed_delivery_methods: {
            title: 'Delivery Methods',
            message: 'Comma-separated delivery methods the agent may request.',
            placeholder: 'auto, direct, propagated'
        }
    };
    return map[key] || defaults;
}

function editAgentPolicyChoice(key) {
    var detail = _settingsAgentsState.detail || {};
    var policy = detail.policy || {};
    var row = document.querySelector('[data-policy-key="' + cssEscapeValue(key) + '"]');
    var choices = row && row.dataset.policyChoices ? row.dataset.policyChoices.split(',').filter(Boolean) : [];
    if (!choices.length) return;
    rsChoice({
        title: 'Agent Guardrail',
        message: key.replace(/_/g, ' '),
        choices: choices.map(function(choice) {
            return {
                label: choice,
                value: choice,
                hint: policy[key] === choice ? 'Current' : ''
            };
        })
    }).then(function(value) {
        if (value === null || value === undefined) return;
        setSelectedAgentPolicy(key, value);
    });
}

function cssEscapeValue(value) {
    if (window.CSS && typeof window.CSS.escape === 'function') return window.CSS.escape(value);
    return String(value).replace(/"/g, '\\"');
}

function agentPresetChoices() {
    var presets = _settingsAgentsState.presets || {};
    var keys = AGENT_PRESET_ORDER.filter(function(key) { return presets[key]; });
    Object.keys(presets).sort().forEach(function(key) {
        if (keys.indexOf(key) === -1) keys.push(key);
    });
    if (!keys.length) keys = AGENT_PRESET_ORDER.slice();
    return keys.map(function(key) {
        var preset = presets[key] || {};
        return {
            label: preset.label || key.replace(/-/g, ' '),
            value: key,
            hint: preset.description || ''
        };
    });
}

function agentAdapterCatalog() {
    var catalog = _settingsAgentsState.adapters || {};
    return catalog.providers || {};
}

function agentAdapterChoices() {
    var providers = agentAdapterCatalog();
    var keys = AGENT_ADAPTER_ORDER.filter(function(key) { return providers[key]; });
    Object.keys(providers).sort().forEach(function(key) {
        if (keys.indexOf(key) === -1) keys.push(key);
    });
    if (!keys.length) keys = AGENT_ADAPTER_ORDER.slice();
    return keys.map(function(key) {
        var provider = providers[key] || {};
        return {
            label: provider.label || key.replace(/-/g, ' '),
            value: key,
            hint: provider.description || ''
        };
    });
}

function adapterDefaults(provider) {
    var meta = agentAdapterCatalog()[provider] || {};
    return {
        label: meta.label || provider.replace(/-/g, ' '),
        base_url: meta.base_url || '',
        secret_env: meta.secret_env || '',
        command: Array.isArray(meta.default_command) ? meta.default_command : []
    };
}

function chooseAgentRuntimeProvider() {
    return rsChoice({
        title: 'Agent Runtime',
        message: 'Choose what will run this Ratspeak agent.',
        choices: agentAdapterChoices()
    });
}

function defaultAgentPresetForProvider(provider) {
    if (provider === 'openclaw') return 'openclaw-basic';
    return 'reply-assistant';
}

function collectAgentAdapterConfig(name, provider) {
    var defaults = adapterDefaults(provider);
    var current = (_settingsAgentsState.detail && _settingsAgentsState.detail.adapter) || {};
    var currentProvider = current.provider === provider ? current : {};
    var config = {
        name: name,
        provider: provider,
        label: currentProvider.label || defaults.label,
        model: currentProvider.model || '',
        base_url: currentProvider.base_url || defaults.base_url || '',
        command: Array.isArray(currentProvider.command) && currentProvider.command.length ? currentProvider.command : defaults.command,
        secret_env: currentProvider.secret && currentProvider.secret.env ? currentProvider.secret.env : (defaults.secret_env || ''),
        notes: currentProvider.notes || ''
    };
    if (provider === 'openclaw') return Promise.resolve(config);
    return rsPrompt({
        title: 'API Key Variable',
        message: 'Environment variable your local adapter will read.',
        placeholder: provider === 'openrouter' ? 'OPENROUTER_API_KEY' : 'VENICE_API_KEY',
        defaultValue: config.secret_env,
        confirmText: 'Next'
    }).then(function(secretEnv) {
        if (secretEnv === null) return null;
        config.secret_env = secretEnv.trim();
        return rsPrompt({
            title: 'Model',
            message: 'Optional model name. You can leave this blank and set it in the adapter.',
            placeholder: provider === 'openrouter' ? 'openai/gpt-4o-mini' : 'model id',
            defaultValue: config.model,
            confirmText: 'Save'
        });
    }).then(function(model) {
        if (model === null) return null;
        config.model = model.trim();
        return Promise.resolve(config);
    });
}

function openAgentCreateFlow() {
    var nameValue = '';
    var providerValue = '';
    rsPrompt({
        title: 'Add Agent',
        message: 'Name this agent profile.',
        placeholder: 'my-agent',
        confirmText: 'Next'
    }).then(function(name) {
        if (name === null) return null;
        nameValue = name.trim();
        if (!nameValue) {
            showToast('Agent name is required', 'toast-red', 2500);
            return null;
        }
        return chooseAgentRuntimeProvider();
    }).then(function(provider) {
        if (!provider) return null;
        providerValue = provider;
        return chooseAgentInitialContact().then(function(contact) {
            return collectAgentAdapterConfig(nameValue, providerValue).then(function(adapterArgs) {
                if (!adapterArgs) return null;
                return {
                    preset: defaultAgentPresetForProvider(providerValue),
                    contact: contact,
                    adapter: adapterArgs
                };
            });
        });
    }).then(function(selection) {
        if (!selection) return;
        var contacts = selection.contact ? [selection.contact] : [];
        var createBtn = document.getElementById('settings-create-agent-btn');
        if (createBtn) createBtn.disabled = true;
        return RS.invoke('create_agent', {
            args: {
                name: nameValue,
                nickname: nameValue,
                preset: selection.preset,
                allowed_contacts: contacts,
                unknown_contacts: 'deny'
            }
        }).then(function(payload) {
            _settingsAgentsState.selected = nameValue;
            return RS.invoke('set_agent_adapter', { args: selection.adapter }).then(function() {
                return payload;
            });
        }).then(function(payload) {
            showToast('Agent created', 'toast-green', 2500);
            loadAgentSettings(true);
            return payload;
        }).catch(function(err) {
            showAgentError(err, 'Failed to create agent');
        }).finally(function() {
            if (createBtn) createBtn.disabled = false;
        });
    });
}

function chooseAgentInitialContact() {
    return RS.invoke('api_contacts').then(function(list) {
        var choices = [{ label: 'No contact yet', value: '', hint: 'Add allowed contacts later from this panel.' }];
        (Array.isArray(list) ? list : []).slice(0, 12).forEach(function(contact) {
            if (!contact || !contact.hash) return;
            choices.push({
                label: contact.display_name || shortHash(contact.hash, 8, 4),
                value: contact.hash,
                hint: shortHash(contact.hash, 8, 4)
            });
        });
        choices.push({ label: 'Paste contact hash', value: 'paste', hint: 'Allow one LXMF destination hash now.' });
        return rsChoice({
            title: 'Allowed Contact',
            message: 'Limit what the agent can read and write.',
            choices: choices
        });
    }).catch(function() {
        return rsChoice({
            title: 'Allowed Contact',
            choices: [
                { label: 'No contact yet', value: '' },
                { label: 'Paste contact hash', value: 'paste' }
            ]
        });
    }).then(function(value) {
        if (value === 'paste') {
            return rsPrompt({
                title: 'Contact Hash',
                message: 'Paste an LXMF destination hash.',
                placeholder: '32 hex characters',
                confirmText: 'Allow'
            }).then(function(hash) { return hash ? hash.trim().toLowerCase() : ''; });
        }
        return value ? String(value).toLowerCase() : '';
    });
}

function addAgentAllowedContact() {
    var name = selectedAgentName();
    if (!name) return;
    chooseAgentInitialContact().then(function(hash) {
        if (!hash) return;
        return RS.invoke('set_agent_grant', {
            args: {
                name: name,
                contacts: [hash],
                replace_contacts: false
            }
        });
    }).then(function(payload) {
        if (!payload) return;
        showToast('Agent contact allowed', 'toast-green', 2000);
        loadAgentDetail(name);
    }).catch(function(err) {
        showAgentError(err, 'Failed to update grant');
    });
}

function editAgentPreset() {
    var name = selectedAgentName();
    if (!name) return;
    rsChoice({
        title: 'Agent Preset',
        message: 'Replace the current scopes with a preset.',
        choices: agentPresetChoices()
    }).then(function(preset) {
        if (!preset) return;
        return RS.invoke('set_agent_grant', {
            args: {
                name: name,
                preset: preset,
                replace_scopes: true
            }
        });
    }).then(function(payload) {
        if (!payload) return;
        showToast('Agent preset updated', 'toast-green', 2000);
        loadAgentDetail(name);
    }).catch(function(err) {
        showAgentError(err, 'Failed to update preset');
    });
}

function configureSelectedAgentAdapter() {
    var name = selectedAgentName();
    if (!name) return;
    chooseAgentRuntimeProvider().then(function(provider) {
        if (!provider) return null;
        return collectAgentAdapterConfig(name, provider);
    }).then(function(args) {
        if (!args) return null;
        return RS.invoke('set_agent_adapter', { args: args });
    }).then(function(payload) {
        if (!payload) return;
        showToast('Agent runtime configured', 'toast-green', 2200);
        loadAgentDetail(name);
        loadAgentSettings(false);
    }).catch(function(err) {
        showAgentError(err, 'Failed to configure runtime');
    });
}

function startSelectedAgentDaemon() {
    var name = selectedAgentName();
    if (!name) return;
    var detail = _settingsAgentsState.detail || {};
    var runtime = detail.runtime || {};
    if (runtime.running) {
        showToast('Agent daemon is already running', 'toast-blue', 2000);
        return;
    }
    RS.invoke('start_agent_daemon', { name: name }).then(function(payload) {
        if (payload && payload.runtime) {
            if (!_settingsAgentsState.detail) _settingsAgentsState.detail = {};
            _settingsAgentsState.detail.runtime = payload.runtime;
            if (payload.connection) _settingsAgentsState.detail.connection = payload.connection;
            renderAgentDetail(_settingsAgentsState.detail);
        }
        showToast('Agent daemon started', 'toast-green', 2500);
        loadAgentSettings(false);
    }).catch(function(err) {
        showAgentError(err, 'Failed to start daemon');
    });
}

function refreshSelectedAgentRuntime() {
    var name = selectedAgentName();
    if (!name) return;
    RS.invoke('api_agent_runtime', { name: name }).then(function(runtime) {
        if (!_settingsAgentsState.detail) _settingsAgentsState.detail = {};
        _settingsAgentsState.detail.runtime = runtime;
        renderAgentDetail(_settingsAgentsState.detail);
        showToast(runtime && runtime.running ? 'Agent daemon running' : 'Agent daemon stopped', runtime && runtime.running ? 'toast-green' : 'toast-blue', 1800);
        loadAgentSettings(false);
    }).catch(function(err) {
        showAgentError(err, 'Failed to refresh runtime');
    });
}

function rotateSelectedAgentToken() {
    var name = selectedAgentName();
    if (!name) return;
    rsConfirm({
        title: 'Rotate Token',
        message: 'Rotate this agent token? Existing agent processes will need to reload the token file.',
        confirmText: 'Rotate'
    }).then(function(ok) {
        if (!ok) return;
        return RS.invoke('rotate_agent_token', { name: name });
    }).then(function(payload) {
        if (!payload) return;
        showToast('Agent token rotated', 'toast-green', 2500);
        loadAgentDetail(name);
    }).catch(function(err) {
        showAgentError(err, 'Failed to rotate token');
    });
}

function revokeSelectedAgent() {
    var name = selectedAgentName();
    if (!name) return;
    rsConfirm({
        title: 'Revoke Agent',
        message: 'Revoke this agent grant? Its daemon API access will be denied after restart/reload.',
        danger: true,
        confirmText: 'Revoke'
    }).then(function(ok) {
        if (!ok) return;
        return RS.invoke('revoke_agent', { name: name, reason: 'revoked from Settings' });
    }).then(function(payload) {
        if (!payload) return;
        showToast('Agent revoked', 'toast-orange', 2500);
        loadAgentSettings(true);
    }).catch(function(err) {
        showAgentError(err, 'Failed to revoke agent');
    });
}

function copyAgentConnectionBundle() {
    var name = selectedAgentName();
    if (!name) return;
    RS.invoke('api_agent_connection_bundle', { name: name }).then(function(bundle) {
        return copyAgentText(JSON.stringify(bundle, null, 2), 'Agent connection kit');
    }).catch(function(err) {
        showAgentError(err, 'Failed to build connection kit');
    });
}

function agentApprovalStateLabel(value) {
    var found = AGENT_APPROVAL_STATES.find(function(state) { return state.value === value; });
    return found ? found.label : 'Actions';
}

function chooseAgentApprovalState() {
    rsChoice({
        title: 'Action Queue',
        message: 'Choose which agent actions to inspect.',
        choices: AGENT_APPROVAL_STATES.map(function(state) {
            return {
                label: state.label,
                value: state.value,
                hint: state.value === _settingsAgentsState.approvalState ? 'Current' : state.hint
            };
        })
    }).then(function(value) {
        if (!value) return;
        _settingsAgentsState.approvalState = value;
        loadAgentApprovals(_settingsAgentsState.selected);
    });
}

function loadAgentApprovals(name) {
    var list = document.getElementById('settings-agent-approvals-list');
    var desc = document.getElementById('settings-agent-approvals-desc');
    var stateBtn = document.getElementById('settings-agent-approval-state-btn');
    var state = _settingsAgentsState.approvalState || 'pending_approval';
    if (stateBtn) stateBtn.textContent = agentApprovalStateLabel(state);
    if (list) list.innerHTML = '<div class="inline-hint">Loading actions...</div>';
    RS.invoke('api_agent_approvals', {
        agent: name || null,
        stateFilter: state
    }).then(function(payload) {
        _settingsAgentsState.approvals = (payload && payload.actions) || [];
        if (desc) {
            var count = _settingsAgentsState.approvals.length;
            desc.textContent = count
                ? (count + ' ' + agentApprovalStateLabel(state).toLowerCase() + ' action' + (count === 1 ? '' : 's') + '.')
                : 'Review pending, approved, cancelled, expired, sent, and failed agent actions.';
        }
        renderAgentApprovals();
    }).catch(function(err) {
        if (list) list.innerHTML = '<div class="inline-error">Failed to load approvals.</div>';
        showAgentError(err, 'Failed to load approvals');
    });
}

function renderAgentApprovals() {
    var list = document.getElementById('settings-agent-approvals-list');
    if (!list) return;
    var approvals = _settingsAgentsState.approvals || [];
    if (!approvals.length) {
        list.innerHTML = '<div class="inline-hint">No matching agent actions.</div>';
        return;
    }
    list.innerHTML = approvals.map(function(action) {
        var files = action.staged_files || [];
        var buttons = agentApprovalButtons(action, files.length > 0);
        return '<div class="settings-agent-approval-row" data-action-id="' + escapeHtml(action.id) + '" data-agent="' + escapeHtml(action.agent || '') + '">' +
            '<div class="settings-agent-approval-main">' +
                '<span class="settings-agent-approval-kind">' + escapeHtml(action.kind || 'action') + '</span>' +
                '<span class="settings-agent-approval-meta">' + escapeHtml(action.state || '') + ' · ' + escapeHtml(action.agent || 'agent') + ' · ' + escapeHtml(shortHash(action.id || '', 8, 4)) + '</span>' +
            '</div>' +
            '<div class="settings-row-actions">' +
                buttons.join('') +
            '</div>' +
        '</div>';
    }).join('');
}

function agentApprovalButtons(action, hasFiles) {
    var state = action.state || _settingsAgentsState.approvalState;
    var buttons = ['<button class="selector-badge selector-badge-no-caret" data-approval-action="review">Review</button>'];
    if (hasFiles) buttons.push('<button class="selector-badge selector-badge-no-caret" data-approval-action="file">File</button>');
    if (state === 'pending_approval') {
        buttons.push('<button class="selector-badge selector-badge-no-caret" data-approval-action="approve">Approve</button>');
        buttons.push('<button class="selector-badge selector-badge-no-caret" data-approval-action="approve-execute">Approve + Run</button>');
        buttons.push('<button class="selector-badge selector-badge-no-caret" data-approval-action="reject">Reject</button>');
        buttons.push('<button class="selector-badge selector-badge-no-caret" data-approval-action="cancel">Cancel</button>');
    } else if (state === 'approved') {
        buttons.push('<button class="selector-badge selector-badge-no-caret" data-approval-action="execute">Run</button>');
        buttons.push('<button class="selector-badge selector-badge-no-caret" data-approval-action="cancel">Cancel</button>');
    } else if (state === 'draft') {
        buttons.push('<button class="selector-badge selector-badge-no-caret" data-approval-action="cancel">Cancel</button>');
    }
    return buttons;
}

function handleAgentApprovalClick(e) {
    var btn = e.target.closest('[data-approval-action]');
    if (!btn) return;
    var row = btn.closest('.settings-agent-approval-row');
    if (!row) return;
    var action = btn.dataset.approvalAction;
    var id = row.dataset.actionId;
    var agent = row.dataset.agent || selectedAgentName();
    if (action === 'review') reviewAgentAction(agent, id);
    else if (action === 'file') inspectAgentActionFile(agent, id);
    else if (action === 'approve') decideAgentAction('approve_agent_action', agent, id, 'Approve action?', false);
    else if (action === 'approve-execute') decideAgentAction('approve_agent_action', agent, id, 'Approve and run this action now?', false, { execute: true });
    else if (action === 'reject') decideAgentAction('reject_agent_action', agent, id, 'Reject action?', true);
    else if (action === 'cancel') decideAgentAction('cancel_agent_action', agent, id, 'Cancel this action?', true);
    else if (action === 'execute') decideAgentAction('execute_agent_action', agent, id, 'Run this approved action now?', false);
}

function reviewAgentAction(agent, id) {
    RS.invoke('api_agent_approval', { agent: agent || null, id: id }).then(function(action) {
        var payload = action && action.payload ? JSON.stringify(action.payload, null, 2) : 'No payload.';
        rsAlert({
            title: 'Agent Action',
            message: (action.kind || 'action') + '\n' + (action.id || id) + '\n\n' + payload,
            closeText: 'Done'
        });
    }).catch(function(err) {
        showAgentError(err, 'Failed to review action');
    });
}

function inspectAgentActionFile(agent, id) {
    RS.invoke('api_agent_file_inspection', {
        args: {
            agent: agent || null,
            id: id,
            preview_bytes: 2000
        }
    }).then(function(payload) {
        var file = payload && payload.file ? payload.file : {};
        var message = [
            file.file_name || 'file',
            file.mime || 'unknown type',
            formatAgentBytes(file.size || 0),
            file.sha256 ? ('sha256 ' + file.sha256) : '',
            file.preview_text ? ('\n' + file.preview_text) : ''
        ].filter(Boolean).join('\n');
        rsAlert({ title: 'Staged File', message: message, closeText: 'Done' });
    }).catch(function(err) {
        showAgentError(err, 'Failed to inspect file');
    });
}

function decideAgentAction(command, agent, id, message, danger, extraArgs) {
    rsConfirm({
        title: danger ? 'Stop Action' : 'Agent Action',
        message: message,
        danger: !!danger,
        confirmText: danger ? 'Continue' : 'Confirm'
    }).then(function(ok) {
        if (!ok) return null;
        var args = {
            agent: agent || null,
            id: id
        };
        if (extraArgs) Object.keys(extraArgs).forEach(function(key) { args[key] = extraArgs[key]; });
        return RS.invoke(command, {
            args: args
        });
    }).then(function(payload) {
        if (!payload) return;
        showToast('Agent action updated', danger ? 'toast-orange' : 'toast-green', 2200);
        loadAgentApprovals(selectedAgentName());
        if (selectedAgentName()) loadAgentDetail(selectedAgentName());
    }).catch(function(err) {
        showAgentError(err, 'Failed to update action');
    });
}

function expireAgentActions() {
    var name = selectedAgentName();
    RS.invoke('expire_agent_actions', { agent: name || null }).then(function(payload) {
        var expired = payload && payload.expired ? payload.expired : 0;
        showToast(expired ? ('Expired ' + expired + ' action' + (expired === 1 ? '' : 's')) : 'No actions expired', expired ? 'toast-orange' : 'toast-blue', 2200);
        loadAgentApprovals(name);
    }).catch(function(err) {
        showAgentError(err, 'Failed to expire actions');
    });
}

function loadAgentAudit(name) {
    var list = document.getElementById('settings-agent-audit-list');
    if (!list) return;
    if (!name) {
        list.innerHTML = '<div class="inline-hint">Agent audit entries appear here.</div>';
        return;
    }
    RS.invoke('api_agent_audit', { agent: name, limit: 8 }).then(function(payload) {
        _settingsAgentsState.audit = (payload && payload.audit) || [];
        renderAgentAudit();
    }).catch(function() {
        list.innerHTML = '<div class="inline-error">Failed to load audit entries.</div>';
    });
}

function renderAgentAudit() {
    var list = document.getElementById('settings-agent-audit-list');
    if (!list) return;
    var audit = _settingsAgentsState.audit || [];
    if (!audit.length) {
        list.innerHTML = '<div class="inline-hint">No audit entries yet.</div>';
        return;
    }
    list.innerHTML = audit.slice().reverse().map(function(entry) {
        return '<div class="settings-agent-audit-row">' +
            '<span class="settings-agent-audit-event">' + escapeHtml(entry.event || 'event') + '</span>' +
            '<span class="settings-agent-audit-meta">' + escapeHtml(entry.actor || '') + ' · ' + escapeHtml(formatAgentTime(entry.created_at_unix)) + '</span>' +
        '</div>';
    }).join('');
}

function formatAgentBytes(bytes) {
    bytes = Number(bytes || 0);
    if (bytes >= 1024 * 1024) return Math.round(bytes / 1024 / 1024) + ' MiB';
    if (bytes >= 1024) return Math.round(bytes / 1024) + ' KiB';
    return bytes + ' B';
}

function formatAgentTime(ts) {
    if (!ts) return '';
    try { return new Date(ts * 1000).toLocaleString(); } catch(e) { return ''; }
}

function copyAgentText(text, label) {
    var done = function() { showToast((label || 'Text') + ' copied', 'toast-green', 1800); };
    if (typeof _copyToClipboard === 'function') {
        return _copyToClipboard(text).then(done);
    }
    if (navigator.clipboard && navigator.clipboard.writeText) {
        return navigator.clipboard.writeText(text).then(done);
    }
    showToast('Clipboard is not available', 'toast-orange', 2500);
    return Promise.resolve(false);
}

function showAgentError(err, fallback) {
    if (typeof showToast !== 'function') return;
    showToast((err && err.message) || fallback || 'Agent request failed', 'toast-red', 5000);
}

function loadSettingsInterfaces() {
    loadSettingsInterfacesWithRetry(1);
}

function loadSettingsInterfacesWithRetry(retries) {
    var container = document.getElementById('settings-interfaces-container');
    if (!container) return;

    container.innerHTML = '<div class="inline-hint">Loading interfaces...</div>';

    RS.invoke('api_hub_interfaces').then(function(ifaces) {
        if (ifaces && ifaces.transport) applyTransportModePayload(ifaces.transport);

        var hasAny = (ifaces.rnode && ifaces.rnode.length) ||
                     (ifaces.auto && ifaces.auto.length) ||
                     (ifaces.tcp_client && ifaces.tcp_client.length) ||
                     (ifaces.tcp_server && ifaces.tcp_server.length) ||
                     (ifaces.backbone_client && ifaces.backbone_client.length) ||
                     (ifaces.backbone_server && ifaces.backbone_server.length);

        var headerEl = document.getElementById('conn-active-header');
        var countEl = document.getElementById('conn-active-count');
        var total = (ifaces.rnode||[]).length + (ifaces.auto||[]).length +
                    (ifaces.tcp_client||[]).length + (ifaces.tcp_server||[]).length +
                    (ifaces.backbone_client||[]).length + (ifaces.backbone_server||[]).length;

        if (!hasAny) {
            container.innerHTML = '';
            if (headerEl) headerEl.style.display = 'none';
            return;
        }

        if (headerEl) headerEl.style.display = '';
        if (countEl) countEl.textContent = total;

        container.innerHTML = '';
        var allRnodes = ifaces.rnode || [];
        var bleIfaces = allRnodes.filter(function(i) { return (i.port || '').indexOf('ble://') === 0; });
        var serialIfaces = allRnodes.filter(function(i) { return (i.port || '').indexOf('ble://') !== 0; });
        renderSettingsIfaceSection(container, 'LoRa Radios', serialIfaces, 'rnode');
        renderSettingsIfaceSection(container, 'BLE Radios', bleIfaces, 'rnode');
        renderSettingsIfaceSection(container, 'Local Network', ifaces.auto || [], 'auto');
        renderSettingsIfaceSection(container, 'TCP Connections', ifaces.tcp_client || [], 'tcp_client');
        renderSettingsIfaceSection(container, 'TCP Servers', ifaces.tcp_server || [], 'tcp_server');
        renderSettingsIfaceSection(container, 'Backbone Connections', ifaces.backbone_client || [], 'backbone_client');
        renderSettingsIfaceSection(container, 'Backbone Servers', ifaces.backbone_server || [], 'backbone_server');
    }).catch(function() {
        if (retries > 0) {
            setTimeout(function() { loadSettingsInterfacesWithRetry(retries - 1); }, 2000);
        } else {
            container.innerHTML = '<div class="inline-error">Failed to load interfaces.</div>';
        }
    });
}

function renderSettingsIfaceSection(parent, title, interfaces, ifaceType) {
    if (interfaces.length === 0) return;

    var section = document.createElement('div');
    section.className = 'settings-iface-section';

    var titleEl = document.createElement('div');
    titleEl.className = 'settings-iface-section-title';
    titleEl.textContent = title;
    section.appendChild(titleEl);

    interfaces.forEach(function(iface) {
        if (RS.ui && typeof RS.ui.createInterfaceRow === 'function') {
            section.appendChild(RS.ui.createInterfaceRow(iface, ifaceType, {
                editable: true,
                disconnectBle: true
            }));
        }
    });

    parent.appendChild(section);
}

var connAddLora = document.getElementById('conn-add-lora');
if (connAddLora) connAddLora.addEventListener('click', function() { openRnodeModal('ble'); });

var connAddTcp = document.getElementById('conn-add-tcp');
if (connAddTcp) connAddTcp.addEventListener('click', function() { openConnectModal(); });

function _isDesktopBackbone() {
    return typeof window !== 'undefined' && !!window.__RATSPEAK_DESKTOP__;
}

var connAddHost = document.getElementById('conn-add-host');
if (connAddHost) connAddHost.addEventListener('click', function() {
    if (!_isDesktopBackbone() || typeof rsChoice !== 'function') {
        openHostModal();
        return;
    }
    rsChoice({
        title: 'Host Server',
        choices: [
            { label: 'TCP Server', value: 'tcp', hint: 'Standard TCP listener for incoming nodes.' },
            { label: 'Backbone Server', value: 'backbone', hint: 'High-throughput Backbone listener for transport-node trunks.' },
        ]
    }).then(function(kind) {
        if (kind === 'tcp') openHostModal();
        else if (kind === 'backbone') openBackboneHostModal();
    });
});

var connToggleLocal = document.getElementById('conn-toggle-local');
if (connToggleLocal) connToggleLocal.addEventListener('click', toggleLocalNetwork);

var connToggleBle = document.getElementById('conn-toggle-ble');
if (connToggleBle) connToggleBle.addEventListener('click', toggleBlePeer);

RS.invoke('api_ble_peer_status').then(function(data) {
    window._blePeerAvailable = !!data.available;
    window._blePeerEnabled = !!data.enabled;
    if (data.state) window._blePeerState = data.state;
    if (typeof data.peer_count === 'number') window._blePeerCount = data.peer_count;
    if (typeof updateBlePeerToggle === 'function') updateBlePeerToggle();
    if (typeof _refreshBlePeerSectionState === 'function') _refreshBlePeerSectionState();
}).catch(function() {});

// Loads before identity.js — cross-file calls MUST use typeof guards.
function openActiveIdentityContactCard() {
    var identityHash = null;
    if (typeof activeIdentityHash !== 'undefined' && activeIdentityHash) {
        identityHash = activeIdentityHash;
    } else if (typeof activeIdentity === 'function') {
        var active = activeIdentity();
        identityHash = active && active.hash ? active.hash : null;
    }
    if (typeof openIdentityShareScreen === 'function') {
        openIdentityShareScreen(identityHash);
    } else if (window.RSContactCard && typeof window.RSContactCard.openIdentityShareScreen === 'function') {
        window.RSContactCard.openIdentityShareScreen(identityHash);
    } else if (typeof showToast === 'function') {
        showToast('Contact card is not ready yet', 'toast-orange', 2500);
    }
}

function settingsCurrentActiveIdentity() {
    if (typeof activeIdentity === 'function') {
        var active = activeIdentity();
        if (active) return active;
    }
    if (typeof activeIdentityHash !== 'undefined' && activeIdentityHash && typeof identityByHash === 'function') {
        return identityByHash(activeIdentityHash);
    }
    return null;
}

function syncSettingsIdentityActions() {
    var active = settingsCurrentActiveIdentity();
    var desc = document.getElementById('settings-active-identity-desc');
    var exportBtn = document.getElementById('settings-backup-identity-btn');
    var phraseBtn = document.getElementById('settings-view-recovery-phrase-btn');
    var activeName = active && typeof identityDisplayName === 'function'
        ? identityDisplayName(active)
        : (active && (active.display_name || active.nickname || active.hash));

    if (desc) {
        desc.textContent = active
            ? ('Active: ' + (activeName || 'Unnamed'))
            : 'No active identity loaded.';
    }

    if (exportBtn) {
        var exportDisabled = !active || !!active.is_hardware;
        exportBtn.disabled = exportDisabled;
        exportBtn.title = !active
            ? 'No active identity loaded'
            : (active.is_hardware ? 'Hardware-key identities cannot be exported' : 'Export active identity');
    }

    if (phraseBtn) {
        var phraseDisabled = !active || !!active.is_hardware || !active.has_mnemonic;
        phraseBtn.disabled = phraseDisabled;
        phraseBtn.title = !active
            ? 'No active identity loaded'
            : (active.is_hardware
                ? 'Hardware-key identities do not have a recovery phrase on this device'
                : (!active.has_mnemonic ? 'No recovery phrase is available for this identity' : 'View active recovery phrase'));
    }

    syncSettingsIdentityStatus();
}
window.syncSettingsIdentityActions = syncSettingsIdentityActions;

function settingsCurrentStatusValue() {
    if (typeof resolveActiveProfileStatus === 'function') {
        return String(resolveActiveProfileStatus() || '').trim();
    }
    return '';
}

function syncSettingsIdentityStatus() {
    var active = settingsCurrentActiveIdentity();
    var desc = document.getElementById('settings-identity-status-desc');
    var editBtn = document.getElementById('settings-edit-status-btn');
    var clearBtn = document.getElementById('settings-clear-status-btn');
    var status = active ? settingsCurrentStatusValue() : '';

    if (desc) {
        desc.textContent = active
            ? (status || 'Not set.')
            : 'No active identity loaded.';
        desc.title = status || '';
    }

    if (editBtn) {
        editBtn.disabled = !active;
        editBtn.title = active ? 'Edit status' : 'No active identity loaded';
    }

    if (clearBtn) {
        clearBtn.disabled = !active || !status;
        clearBtn.title = !active
            ? 'No active identity loaded'
            : (status ? 'Clear status' : 'No status to clear');
    }
}

function clearActiveIdentityStatus() {
    if (!settingsCurrentActiveIdentity() || typeof saveIdentityStatus !== 'function') return;
    var clearBtn = document.getElementById('settings-clear-status-btn');
    var editBtn = document.getElementById('settings-edit-status-btn');
    if (clearBtn && clearBtn.disabled) return;

    if (clearBtn) clearBtn.disabled = true;
    if (editBtn) editBtn.disabled = true;

    saveIdentityStatus('').then(function(result) {
        var savedStatus = typeof profileStatusFromPayload === 'function'
            ? profileStatusFromPayload(result)
            : '';
        setActiveProfileStatus(savedStatus === null ? '' : savedStatus);
        if (typeof showToast === 'function') showToast('Status cleared', 'toast-green', 2500);
        if (typeof loadIdentities === 'function') loadIdentities();
    }).catch(function(err) {
        if (typeof showToast === 'function') {
            showToast((err && err.message) ? err.message : 'Failed to clear status', 'toast-red', 3000);
        }
        syncSettingsIdentityStatus();
    });
}

var PROFILE_STATUS_MAX_BYTES = 50;
var _activeProfileStatus = '';

function profileStatusFromPayload(data) {
    if (!data || typeof data !== 'object') return null;
    if (Object.prototype.hasOwnProperty.call(data, 'status')) {
        return data.status == null ? '' : String(data.status);
    }
    if (Object.prototype.hasOwnProperty.call(data, 'profile_status')) {
        return data.profile_status == null ? '' : String(data.profile_status);
    }
    return null;
}

function profileStatusByteLength(value) {
    value = value || '';
    if (window.TextEncoder) return new TextEncoder().encode(value).length;
    return new Blob([value]).size;
}

function trimProfileStatusToByteLimit(value, limit) {
    value = String(value || '');
    limit = limit || PROFILE_STATUS_MAX_BYTES;
    if (profileStatusByteLength(value) <= limit) return value;

    var out = '';
    var bytes = 0;
    for (var i = 0; i < value.length;) {
        var code = value.charCodeAt(i);
        var ch = value.charAt(i);
        if (code >= 0xD800 && code <= 0xDBFF && i + 1 < value.length) {
            var next = value.charCodeAt(i + 1);
            if (next >= 0xDC00 && next <= 0xDFFF) {
                ch = value.substring(i, i + 2);
                i += 2;
            } else {
                i++;
            }
        } else {
            i++;
        }
        var chBytes = profileStatusByteLength(ch);
        if (bytes + chBytes > limit) break;
        out += ch;
        bytes += chBytes;
    }
    return out;
}

function resolveActiveProfileStatus(explicitStatus) {
    if (typeof explicitStatus === 'string') return trimProfileStatusToByteLimit(explicitStatus, PROFILE_STATUS_MAX_BYTES);
    if (typeof activeIdentity === 'function') {
        var active = activeIdentity();
        var activeStatus = profileStatusFromPayload(active);
        if (activeStatus !== null) return trimProfileStatusToByteLimit(activeStatus, PROFILE_STATUS_MAX_BYTES);
    }
    if (typeof lxmfIdentity !== 'undefined' && lxmfIdentity) {
        var lxmfStatus = profileStatusFromPayload(lxmfIdentity);
        if (lxmfStatus !== null) return trimProfileStatusToByteLimit(lxmfStatus, PROFILE_STATUS_MAX_BYTES);
    }
    return _activeProfileStatus || '';
}

function ensureProfileStatusText(parent, id, tagName, className, beforeEl) {
    var existing = document.getElementById(id);
    if (existing) return existing;
    if (!parent) return null;
    var el = document.createElement(tagName || 'span');
    el.id = id;
    el.className = className + ' profile-status-text profile-status-empty';
    el.textContent = 'Set a status';
    if (beforeEl && beforeEl.parentNode === parent) parent.insertBefore(el, beforeEl);
    else parent.appendChild(el);
    return el;
}

function ensureProfileStatusElements() {
    var headerInfo = document.getElementById('header-mobile-info') || document.querySelector('.header-mobile-info');
    if (headerInfo && !headerInfo.id) headerInfo.id = 'header-mobile-info';
    ensureProfileStatusText(
        headerInfo,
        'header-mobile-status',
        'span',
        'header-mobile-status',
        null
    );

    var sidebarMeta = document.querySelector('.sidebar-identity-meta');
    ensureProfileStatusText(
        sidebarMeta,
        'sidebar-identity-status',
        'div',
        'sidebar-identity-status',
        document.getElementById('sidebar-identity-hash')
    );

    var msgProfileInfo = document.querySelector('.msg-profile-info');
    ensureProfileStatusText(
        msgProfileInfo,
        'msg-profile-status',
        'span',
        'msg-profile-status',
        document.getElementById('lxmf-own-hash')
    );
}

function updateProfileStatusElement(el, status) {
    if (!el) return;
    var value = status || '';
    el.textContent = value || 'Set a status';
    el.classList.toggle('profile-status-empty', !value);
    el.title = value ? 'Edit status' : 'Set a status';
}

function renderActiveProfileStatus(status) {
    ensureProfileStatusElements();
    var value = trimProfileStatusToByteLimit(status || '', PROFILE_STATUS_MAX_BYTES);
    updateProfileStatusElement(document.getElementById('header-mobile-status'), value);
    updateProfileStatusElement(document.getElementById('sidebar-identity-status'), value);
    updateProfileStatusElement(document.getElementById('msg-profile-status'), value);
    syncSettingsIdentityStatus();
}

function setActiveProfileStatus(status) {
    _activeProfileStatus = trimProfileStatusToByteLimit(status || '', PROFILE_STATUS_MAX_BYTES);
    renderActiveProfileStatus(_activeProfileStatus);

    if (typeof activeIdentity === 'function') {
        var active = activeIdentity();
        if (active) active.status = _activeProfileStatus;
    }
    if (typeof lxmfIdentity !== 'undefined' && lxmfIdentity) {
        lxmfIdentity.status = _activeProfileStatus;
    }
}

function syncActiveProfileStatusFromPayload(data) {
    var status = profileStatusFromPayload(data);
    if (status !== null) setActiveProfileStatus(status);
    else renderActiveProfileStatus(_activeProfileStatus);
}

function wireProfileStatusEditorTrigger(el) {
    if (!el || el._profileStatusEditorWired) return;
    el._profileStatusEditorWired = true;
    el.title = el.title || 'Edit status';
    el.addEventListener('click', function(e) {
        e.stopPropagation();
        if (typeof openIdentityStatusEditor === 'function') openIdentityStatusEditor();
    });
    el.addEventListener('keydown', function(e) {
        if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault();
            e.stopPropagation();
            if (typeof openIdentityStatusEditor === 'function') openIdentityStatusEditor();
        }
    });
}

function wireProfileStatusEditorTriggers() {
    ensureProfileStatusElements();
    wireProfileStatusEditorTrigger(document.getElementById('header-mobile-info'));
    wireProfileStatusEditorTrigger(document.getElementById('msg-profile-name'));
    wireProfileStatusEditorTrigger(document.getElementById('msg-profile-status'));
    wireProfileStatusEditorTrigger(document.getElementById('sidebar-identity-status'));
}

function saveIdentityStatus(nextStatus) {
    return RS.invoke('set_identity_status', { status: nextStatus });
}

function openIdentityStatusEditor() {
    if (typeof _rsBuildSheet !== 'function') return;

    var initialStatus = resolveActiveProfileStatus();
    var built = _rsBuildSheet({}, function() {});

    built.overlay.addEventListener('click', function(e) {
        if (e.target === built.overlay) built.dismiss(null);
    });

    var label = document.createElement('label');
    label.className = 'rs-dialog-field-label';
    label.textContent = 'Status';

    var textarea = document.createElement('textarea');
    textarea.className = 'rs-dialog-input profile-status-input';
    textarea.placeholder = 'Set a status';
    textarea.rows = 3;
    textarea.value = initialStatus;
    disableAutoCorrect(textarea);

    var meta = document.createElement('div');
    meta.className = 'profile-status-editor-meta';
    var counter = document.createElement('span');
    counter.className = 'profile-status-counter';
    meta.appendChild(counter);

    function updateCounter() {
        var trimmed = trimProfileStatusToByteLimit(textarea.value, PROFILE_STATUS_MAX_BYTES);
        if (trimmed !== textarea.value) textarea.value = trimmed;
        var bytes = profileStatusByteLength(textarea.value);
        counter.textContent = bytes + '/' + PROFILE_STATUS_MAX_BYTES;
        counter.classList.toggle('at-limit', bytes >= PROFILE_STATUS_MAX_BYTES);
    }

    textarea.addEventListener('input', updateCounter);
    updateCounter();

    built.body.classList.add('profile-status-editor-body');
    built.body.appendChild(label);
    built.body.appendChild(textarea);
    built.body.appendChild(meta);

    var cancelBtn = document.createElement('button');
    cancelBtn.className = 'rs-dialog-cancel';
    cancelBtn.textContent = 'Cancel';
    cancelBtn.addEventListener('click', function() { built.dismiss(null); });

    var saveBtn = document.createElement('button');
    saveBtn.className = 'rs-dialog-confirm';
    saveBtn.textContent = 'Save';
    saveBtn.addEventListener('click', function() {
        var nextStatus = trimProfileStatusToByteLimit(textarea.value.trim(), PROFILE_STATUS_MAX_BYTES);
        textarea.value = nextStatus;
        updateCounter();
        saveBtn.disabled = true;
        cancelBtn.disabled = true;
        saveBtn.textContent = 'Saving...';
        saveIdentityStatus(nextStatus).then(function(result) {
            var savedStatus = profileStatusFromPayload(result);
            setActiveProfileStatus(savedStatus === null ? nextStatus : savedStatus);
            built.dismiss(nextStatus);
            if (typeof showToast === 'function') showToast('Status saved', 'toast-green', 2500);
            if (typeof loadIdentities === 'function') loadIdentities();
        }).catch(function(err) {
            saveBtn.disabled = false;
            cancelBtn.disabled = false;
            saveBtn.textContent = 'Save';
            if (typeof showToast === 'function') {
                showToast((err && err.message) ? err.message : 'Failed to save status', 'toast-red', 3000);
            }
        });
    });

    built.footer.appendChild(cancelBtn);
    built.footer.appendChild(saveBtn);

    built.sheet.addEventListener('keydown', function(e) {
        if (e.key === 'Escape') {
            e.stopPropagation();
            built.dismiss(null);
        }
        if ((e.key === 'Enter' && (e.metaKey || e.ctrlKey)) && !saveBtn.disabled) {
            e.preventDefault();
            saveBtn.click();
        }
        if (e.key === 'Tab') {
            var focusable = built.sheet.querySelectorAll('textarea, button');
            if (!focusable.length) return;
            var first = focusable[0];
            var last = focusable[focusable.length - 1];
            if (e.shiftKey && document.activeElement === first) {
                e.preventDefault();
                last.focus();
            } else if (!e.shiftKey && document.activeElement === last) {
                e.preventDefault();
                first.focus();
            }
        }
    });

    if (RS.gestures && typeof RS.gestures.attachDragDismiss === 'function') {
        RS.gestures.attachDragDismiss(built.sheet, {
            axis: 'y',
            blockIfScrolled: true,
            skipIf: function(e) {
                return !!(e.target.closest('button') || e.target.tagName === 'TEXTAREA');
            },
            parallaxOverlay: built.overlay,
            onCommit: function() { built.dismiss(null); }
        });
    }

    built.present();

    if (typeof isMobile !== 'function' || !isMobile()) {
        textarea.focus();
        textarea.select();
    }
}

function updateHeaderIdentity(hash, displayName, status) {
    var resolvedStatus = resolveActiveProfileStatus(status);
    setActiveProfileStatus(resolvedStatus);
    wireProfileStatusEditorTriggers();

    var pill = document.getElementById('header-identity-pill');
    var iconEl = document.getElementById('header-identity-icon');
    var hashEl = document.getElementById('header-identity-hash');
    if (hash && pill) {
        if (iconEl) iconEl.innerHTML = (typeof identityAvatar === 'function') ? identityAvatar(hash, 20) : '';
        if (hashEl) {
            hashEl.textContent = hash.substring(0, 8) + '\u2026';
            hashEl.dataset.full = hash;
        }
        pill.classList.remove('hidden');
        if (!pill._copyWired) {
            pill._copyWired = true;
            pill.addEventListener('click', function() {
                openActiveIdentityContactCard();
            });
        }
    }
    var sidebarId = document.getElementById('sidebar-identity');
    var sidebarIcon = document.getElementById('sidebar-identity-icon');
    var sidebarName = document.getElementById('sidebar-identity-name');
    var sidebarHash = document.getElementById('sidebar-identity-hash');
    if (hash && sidebarId) {
        if (sidebarIcon) sidebarIcon.innerHTML = (typeof identityAvatar === 'function') ? identityAvatar(hash, 32) : '';
        var resolvedName = displayName || localStorage.getItem('ratspeak_identity_name') || 'Unnamed';
        if (sidebarName) sidebarName.textContent = resolvedName;
        if (sidebarHash) {
            sidebarHash.textContent = hash.substring(0, 8) + '\u2026' + hash.substring(hash.length - 4);
            sidebarHash.dataset.full = hash;
        }
        var openSidebarIdentity = function() {
            if (typeof switchView === 'function') switchView('identity');
        };
        if (!sidebarId._wired) {
            sidebarId._wired = true;
            sidebarId.addEventListener('click', openSidebarIdentity);
            sidebarId.addEventListener('keydown', function(e) {
                if (e.key === 'Enter' || e.key === ' ') {
                    e.preventDefault();
                    openSidebarIdentity();
                }
            });
        }
    }
    var lxmfHash = document.getElementById('lxmf-own-hash');
    if (lxmfHash && hash) {
        lxmfHash.textContent = hash.substring(0, 8) + '\u2026' + hash.substring(hash.length - 4);
        lxmfHash.title = 'Click to copy: ' + hash;
        lxmfHash.dataset.full = hash;
    }
    var hdrAvatar = document.getElementById('header-mobile-avatar');
    var hdrName = document.getElementById('header-mobile-name');
    if (hash && hdrAvatar) hdrAvatar.innerHTML = (typeof identityAvatar === 'function') ? identityAvatar(hash, 36) : '';
    if (hdrName) hdrName.textContent = displayName || localStorage.getItem('ratspeak_identity_name') || 'Account 1';
    renderActiveProfileStatus(resolvedStatus);

    // JS fallback for WebView CSS caching. Header profile controls no longer
    // include chevrons; sidebar identity management keeps its switch affordance.
    var _chevrons = document.querySelectorAll('.header-identity-chevron');
    var _showChevron = typeof identityList !== 'undefined' && identityList.length > 1;
    for (var ci = 0; ci < _chevrons.length; ci++) {
        _chevrons[ci].style.display = _showChevron ? '' : 'none';
    }

    var mobileId = document.getElementById('header-mobile-identity');
    if (mobileId && !mobileId._wired) {
        mobileId._wired = true;
        mobileId.addEventListener('click', function() {
            openActiveIdentityContactCard();
        });
        mobileId.addEventListener('keydown', function(e) {
            if (e.key === 'Enter' || e.key === ' ') {
                e.preventDefault();
                openActiveIdentityContactCard();
            }
        });
    }
}

// Skip while setup is still being checked so factory reset doesn't paint stale identity.
if (_cachedIdentityHash && !document.body.classList.contains('checking-setup')) {
    updateHeaderIdentity(_cachedIdentityHash);
}

RS.invoke('api_identity').then(function(data) {
    if (data.exists === false) return;

    try {
        if (data.display_name) {
            localStorage.setItem('ratspeak_identity_name', data.display_name);
        }
        if (data.lxmf_destination) {
            updateHeaderIdentity(data.lxmf_destination, data.display_name, profileStatusFromPayload(data));
            localStorage.setItem('ratspeak_identity_hash', data.lxmf_destination);
        }
        if (data.hash) {
            lxmfIdentityHash = data.hash;
        }
    } catch(e) {
        window.RS.diag('error', '[Settings] Error processing identity data:', e);
    }
}).catch(function(err) {
    window.RS.diag('error', '[Settings] Failed to load identity:', err);
});

var portEl = document.getElementById('settings-port');
if (portEl) {
    portEl.textContent = window.location.port || (window.location.protocol === 'https:' ? '443' : '80');
}

// Identity switches reload conversations themselves; skip the duplicate here.
RS.listen('lxmf_identity', function(data) {
    var h = data.lxmf_hash || data.hash;
    if (h) {
        if (data.display_name) localStorage.setItem('ratspeak_identity_name', data.display_name);
        updateHeaderIdentity(h, data.display_name, profileStatusFromPayload(data));
        localStorage.setItem('ratspeak_identity_hash', h);
        if (!window._identitySwitchInProgress && typeof loadConversations === 'function') {
            loadConversations();
        }
    } else {
        syncActiveProfileStatusFromPayload(data);
    }
});

function applyTransportModePayload(data) {
    if (RS.ui && typeof RS.ui.applyTransportModePayload === 'function') {
        RS.ui.applyTransportModePayload('transport-mode-select', data, { toastSuppressed: true });
    }
}

var _settingsTransportBadge = document.getElementById('transport-mode-select');
if (_settingsTransportBadge) {
    function _openTransportChoice() {
        if (RS.ui && typeof RS.ui.openTransportModeChoice === 'function') {
            RS.ui.openTransportModeChoice(_settingsTransportBadge);
        }
    }

    if (RS.ui && typeof RS.ui.bindTransportChoice === 'function') {
        RS.ui.bindTransportChoice(_settingsTransportBadge);
    } else {
        _settingsTransportBadge.addEventListener('click', _openTransportChoice);
        _settingsTransportBadge.addEventListener('keydown', function(e) {
            if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); _openTransportChoice(); }
        });
    }
}

// Network-change detection is native (NetworkCallback / NWPathMonitor invoking
// `RS.invoke('network_type_changed', ...)`); WKWebView lacks navigator.connection.

RS.listen('transport_mode_updated', function(data) {
    applyTransportModePayload(data);
});

var _announceLabels = {
    0: 'Never',
    900: '15 min',
    1800: '30 min',
    3600: '1 hr'
};

function _announceLabel(secs) {
    secs = parseInt(secs, 10) || 0;
    if (_announceLabels[secs] !== undefined) return _announceLabels[secs];
    if (secs <= 0) return 'Never';
    var hours = secs / 3600;
    if (hours >= 1 && hours === Math.floor(hours)) return hours + 'h';
    return Math.round(secs / 60) + ' min';
}

var _settingsAnnounceBadge = document.getElementById('auto-announce-select');
if (_settingsAnnounceBadge) {
    function _openAnnounceChoice() {
        rsChoice({
            title: 'Auto-Announce',
            message: 'Automatically announce your presence every:',
            choices: [
                { label: 'Never', value: '0', hint: 'Only announce manually.' },
                { label: '15 minutes', value: '900', hint: 'Recommended for active mesh networks.' },
                { label: '30 minutes', value: '1800', hint: 'Good balance of visibility and efficiency.' },
                { label: '1 hour', value: '3600', hint: 'Low-traffic, long-running nodes.' },
                { label: 'Custom\u2026', value: 'custom', hint: 'Set a custom interval (1\u201348 hours).' }
            ]
        }).then(function(val) {
            if (val === null) return;
            if (val === 'custom') {
                return rsPrompt({
                    title: 'Custom Interval',
                    message: 'Enter interval in hours (1\u201348):',
                    placeholder: 'e.g. 2',
                    confirmText: 'Set'
                }).then(function(input) {
                    if (input === null || input.trim() === '') return null;
                    var hours = parseInt(input, 10);
                    if (isNaN(hours) || hours < 1) hours = 1;
                    if (hours > 48) hours = 48;
                    return String(hours * 3600);
                });
            }
            return val;
        }).then(function(secs) {
            if (secs === null || secs === undefined) return;
            var interval = parseInt(secs, 10);
            _settingsAnnounceBadge.textContent = _announceLabel(interval);
            _settingsAnnounceBadge.setAttribute('data-value', interval);
            RS.invoke('set_auto_announce', { interval: interval }).catch(function(err) {
                showToast((err && err.message) || 'Failed to update announce interval', 'toast-red', 8000);
            });
        });
    }

    _settingsAnnounceBadge.addEventListener('click', _openAnnounceChoice);
    _settingsAnnounceBadge.addEventListener('keydown', function(e) {
        if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); _openAnnounceChoice(); }
    });
}

RS.listen('auto_announce_updated', function(data) {
    applyAppSettingsPayload({ auto_announce_interval: data && data.interval });
});

function applyAppSettingsPayload(data) {
    if (!data) return;
    var badge = document.getElementById('auto-announce-select');
    var interval = data.auto_announce_interval !== undefined ? data.auto_announce_interval : data.interval;
    if (badge && interval !== undefined) {
        var secs = parseInt(interval, 10);
        badge.textContent = _announceLabel(secs);
        badge.setAttribute('data-value', secs);
    }
    var usageToggle = document.getElementById('announce-ratspeak-usage-toggle');
    if (usageToggle && data.announce_ratspeak_usage !== undefined) {
        usageToggle.checked = !!data.announce_ratspeak_usage;
    }
    var hwBadge = document.getElementById('hw-lock-timeout-select');
    if (hwBadge && data.hardware_session_timeout !== undefined) {
        var t = parseInt(data.hardware_session_timeout, 10);
        hwBadge.textContent = _hwLockLabel(t);
        hwBadge.setAttribute('data-value', t);
    }
}

function _hwLockLabel(secs) {
    if (!secs || secs <= 0) return 'Off';
    if (secs % 3600 === 0) { var h = secs / 3600; return h + (h === 1 ? ' hour' : ' hours'); }
    if (secs % 60 === 0) return (secs / 60) + ' min';
    return secs + 's';
}

// Reveal the Hardware Key Auto-Lock row only when a hardware identity exists.
function _maybeRevealHwLockRow() {
    var row = document.getElementById('hw-lock-row');
    if (!row) return;
    RS.invoke('api_list_identities').then(function(list) {
        var hasHw = Array.isArray(list) && list.some(function(i) { return i && i.is_hardware; });
        row.style.display = hasHw ? '' : 'none';
    }).catch(function() {});
}

function _initHwLockSetting() {
    var badge = document.getElementById('hw-lock-timeout-select');
    if (!badge) return;
    function open() {
        rsChoice({
            title: 'Hardware Key Auto-Lock',
            message: 'Lock your hardware identity after this much idle time. You’ll re-enter the PIN to resume.',
            choices: [
                { label: 'Off', value: '0', hint: 'Only locks when you quit Ratspeak.' },
                { label: '5 minutes', value: '300', hint: 'Tightest; frequent PIN prompts.' },
                { label: '15 minutes', value: '900' },
                { label: '30 minutes', value: '1800' },
                { label: '1 hour', value: '3600' }
            ]
        }).then(function(val) {
            if (val === null || val === undefined) return;
            var secs = parseInt(val, 10);
            badge.textContent = _hwLockLabel(secs);
            badge.setAttribute('data-value', secs);
            RS.invoke('set_hardware_lock_timeout', { seconds: secs }).catch(function(err) {
                showToast((err && err.message) || 'Failed to update auto-lock', 'toast-red', 8000);
            });
        });
    }
    badge.addEventListener('click', open);
    badge.addEventListener('keydown', function(e) {
        if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); open(); }
    });
}

document.addEventListener('DOMContentLoaded', function() {
    _initHwLockSetting();
    _maybeRevealHwLockRow();
});

(function() {
    var usageToggle = document.getElementById('announce-ratspeak-usage-toggle');
    RS.invoke('api_app_settings').then(applyAppSettingsPayload).catch(function() {});
    if (!usageToggle) return;
    usageToggle.addEventListener('change', function() {
        var enabled = !!usageToggle.checked;
        RS.invoke('set_announce_ratspeak_usage', { enabled: enabled })
            .then(function(data) {
                if (data && data.enabled !== undefined) usageToggle.checked = !!data.enabled;
            })
            .catch(function(err) {
                usageToggle.checked = !enabled;
                showToast((err && err.message) || 'Failed to update privacy setting', 'toast-red', 8000);
            });
    });
})();

RS.listen('app_settings_updated', applyAppSettingsPayload);

// Keep this desktop-only until mobile has a user-facing notifications screen.
(function() {
    var _notifRow = document.getElementById('settings-row-notifications');
    var _notifToggle = document.getElementById('desktop-notifications-toggle');
    if (!_notifRow || !_notifToggle) return;
    var _isMobile = (typeof isMobile === 'function') ? isMobile() : !!window.__RATSPEAK_MOBILE__;
    if (_isMobile) return;
    _notifRow.style.display = '';
    RS.invoke('api_notification_settings').then(function(data) {
        if (!data || data.enabled === undefined) return;
        _notifToggle.checked = !!data.enabled;
        if (typeof rsNotify !== 'undefined') rsNotify.setEnabled(!!data.enabled);
        if (data.enabled && typeof rsNotify !== 'undefined' && rsNotify.available()) {
            rsNotify.requestPermission();
        }
    }).catch(function() {});

    _notifToggle.addEventListener('change', function() {
        var enabled = !!_notifToggle.checked;
        if (typeof rsNotify !== 'undefined') rsNotify.setEnabled(enabled);
        RS.invoke('set_desktop_notifications', { enabled: enabled }).catch(function() {});
        if (enabled && typeof rsNotify !== 'undefined' && rsNotify.available()) {
            rsNotify.requestPermission();
        }
    });
})();

RS.listen('desktop_notifications_updated', function(data) {
    var toggle = document.getElementById('desktop-notifications-toggle');
    if (!toggle || !data || data.enabled === undefined) return;
    toggle.checked = !!data.enabled;
    if (typeof rsNotify !== 'undefined') rsNotify.setEnabled(!!data.enabled);
});

var settingsCreateBtn = document.getElementById('settings-create-identity-btn');
if (settingsCreateBtn) settingsCreateBtn.addEventListener('click', function() {
    if (typeof createNewIdentity === 'function') createNewIdentity();
});

var settingsImportBtn = document.getElementById('settings-import-identity-btn');
if (settingsImportBtn) settingsImportBtn.addEventListener('click', function() {
    if (typeof importIdentity === 'function') importIdentity();
});

var settingsBackupBtn = document.getElementById('settings-backup-identity-btn');
if (settingsBackupBtn) settingsBackupBtn.addEventListener('click', function() {
    if (typeof exportActiveIdentity === 'function') exportActiveIdentity();
});

var settingsViewPhraseBtn = document.getElementById('settings-view-recovery-phrase-btn');
if (settingsViewPhraseBtn) settingsViewPhraseBtn.addEventListener('click', function() {
    if (typeof viewActiveRecoveryPhrase === 'function') viewActiveRecoveryPhrase();
    else if (typeof showToast === 'function') showToast('Recovery phrase is not ready yet', 'toast-orange', 2500);
});

var settingsEditStatusBtn = document.getElementById('settings-edit-status-btn');
if (settingsEditStatusBtn) settingsEditStatusBtn.addEventListener('click', function() {
    if (settingsEditStatusBtn.disabled) return;
    if (typeof openIdentityStatusEditor === 'function') openIdentityStatusEditor();
});

var settingsClearStatusBtn = document.getElementById('settings-clear-status-btn');
if (settingsClearStatusBtn) settingsClearStatusBtn.addEventListener('click', clearActiveIdentityStatus);

var _manageIdentitiesBtn = document.getElementById('settings-manage-identities-btn');
if (_manageIdentitiesBtn) {
    _manageIdentitiesBtn.addEventListener('click', function() {
        if (typeof switchView === 'function') switchView('identity');
    });
}

syncSettingsIdentityActions();

function clearWithConfirm(commandName, confirmMsg, successMsg, failMsg) {
    var errorMsg = failMsg || 'Operation failed.';
    rsConfirm({ message: confirmMsg, danger: true, confirmText: 'Clear' }).then(function(ok) {
        if (!ok) return;
        RS.invoke(commandName).then(function() {
            showToast(successMsg, '', 3000);
        }).catch(function() {
            showToast(errorMsg, 'toast-red', 3000);
        });
    });
}

var clearPathsBtn = document.getElementById('settings-clear-paths');
if (clearPathsBtn) {
    clearPathsBtn.addEventListener('click', function() {
        clearWithConfirm('api_clear_paths',
            'Clear all cached paths? Paths will be re-discovered over time.',
            'Path table cleared.',
            'Failed to clear paths.');
    });
}

var clearAnnouncesBtn = document.getElementById('settings-clear-announces');
if (clearAnnouncesBtn) {
    clearAnnouncesBtn.addEventListener('click', function() {
        clearWithConfirm('api_clear_announces',
            'Clear announce history?',
            'Announce history cleared.',
            'Failed to clear announce history.');
    });
}

var clearMessagesBtn = document.getElementById('settings-clear-messages');
if (clearMessagesBtn) {
    clearMessagesBtn.addEventListener('click', function() {
        clearWithConfirm('api_clear_messages',
            'Delete ALL messages? This cannot be undone.',
            'All messages deleted.',
            'Failed to delete messages.');
    });
}

var clearContactsBtn = document.getElementById('settings-clear-contacts');
if (clearContactsBtn) {
    clearContactsBtn.addEventListener('click', function() {
        clearWithConfirm('api_clear_contacts',
            'Delete ALL contacts? This cannot be undone.',
            'All contacts deleted.',
            'Failed to delete contacts.');
    });
}

var resetDatabaseBtn = document.getElementById('settings-reset-database');
if (resetDatabaseBtn) {
    resetDatabaseBtn.addEventListener('click', function() {
        clearWithConfirm('api_reset_database',
            'Clear ALL messages and contacts? This cannot be undone.',
            'All messages and contacts cleared.',
            'Failed to clear data.');
    });
}

var factoryResetBtn = document.getElementById('settings-factory-reset');
if (factoryResetBtn) {
    factoryResetBtn.addEventListener('click', function() {
        if (factoryResetBtn.disabled) return;
        factoryResetBtn.disabled = true;
        // Defer past the tap — WKWebView focus()-in-touch-handler stalls main thread.
        setTimeout(function() {
            try {
                confirmDangerAction('factory-reset', function onClose() {
                    factoryResetBtn.disabled = false;
                });
            } catch (e) {
                factoryResetBtn.disabled = false;
                throw e;
            }
        }, 0);
    });
}

var _lastAnnounceTime = 0;
var ANNOUNCE_COOLDOWN = 5000;
var _announceCooldownTimer = null;

function setAnnounceLabel(btn, text) {
    if (!btn) return;
    var labelEl = btn.querySelector('span:not([aria-hidden])') || btn.querySelector('span');
    if (labelEl) labelEl.textContent = text;
    else btn.textContent = text;
}

// Returns true if IPC fired, false if rate-limited or no online interface.
function tryTriggerAnnounce() {
    if (Date.now() - _lastAnnounceTime < ANNOUNCE_COOLDOWN) {
        showRateLimitedToast();
        return false;
    }
    if (_anyInterfaceOnline === false) {
        showToast('Connect to a network first!', 'toast-orange', 3000);
        return false;
    }
    RS.invoke('trigger_announce').catch(function(err) {
        showToast((err && err.message) || 'Failed to send announce', 'toast-red', 8000);
    });
    return true;
}

RS.listen('announce_triggered', function(data) {
    var networkBtn = document.getElementById('network-announce-btn');
    if (networkBtn && networkBtn.dataset) delete networkBtn.dataset.announcePending;
    // Pop the long-press origin (nav.js _holdLoop); ignore if stale (>5s).
    var origin = (typeof _pendingAnnounceOrigin !== 'undefined') ? _pendingAnnounceOrigin : null;
    if (origin && Date.now() - origin.t > 5000) origin = null;
    if (typeof _pendingAnnounceOrigin !== 'undefined') _pendingAnnounceOrigin = null;

    if (data.success) {
        _lastAnnounceTime = Date.now();
        if (typeof haptic === 'function') haptic('success');
        showToast('Announcement sent!', 'toast-green', 4000);
        // Burst is gated on backend success so it aligns with the real outcome.
        if (origin && typeof showAnnounceAnimation === 'function') {
            showAnnounceAnimation(origin.el, origin.cx, origin.cy);
        }
        if (networkBtn) {
            setAnnounceLabel(networkBtn, 'Announced!');
            networkBtn.classList.add('is-success');
            setTimeout(function() {
                setAnnounceLabel(networkBtn, 'Announce');
                networkBtn.classList.remove('is-success');
                networkBtn.classList.add('is-cooldown');
                networkBtn.disabled = true;
            }, 2000);
            if (_announceCooldownTimer) clearTimeout(_announceCooldownTimer);
            _announceCooldownTimer = setTimeout(function() {
                networkBtn.classList.remove('is-cooldown');
                networkBtn.disabled = false;
                _announceCooldownTimer = null;
            }, ANNOUNCE_COOLDOWN);
        }
    } else if (data.error === 'no_interfaces') {
        if (typeof haptic === 'function') haptic('warning');
        showToast('Connect to a network first!', 'toast-orange', 3000);
        // Frontend cache disagreed with backend; play dampened animation for closure.
        if (origin && typeof showAnnounceFailAnimation === 'function') {
            showAnnounceFailAnimation(origin.el, origin.cx, origin.cy);
        }
        if (networkBtn) {
            setAnnounceLabel(networkBtn, 'Announce');
            networkBtn.disabled = false;
        }
    } else if (data.error === 'not_sent') {
        if (typeof haptic === 'function') haptic('warning');
        var announceMsg = window._autoEnabled
            ? 'Announce queued, but no interface transmitted it yet. Local Network may still be finding peers.'
            : 'Announce queued, but no connected interface transmitted it. Check that your TCP peer is connected or enable Local Network.';
        showToast(announceMsg, 'toast-orange', 5000);
        if (origin && typeof showAnnounceFailAnimation === 'function') {
            showAnnounceFailAnimation(origin.el, origin.cx, origin.cy);
        }
        if (networkBtn) {
            setAnnounceLabel(networkBtn, 'Announce');
            networkBtn.disabled = false;
        }
    } else {
        if (typeof haptic === 'function') haptic('error');
        showToast('Announce failed — router not ready', 'toast-red', 4000);
        if (origin && typeof showAnnounceFailAnimation === 'function') {
            showAnnounceFailAnimation(origin.el, origin.cx, origin.cy);
        }
        if (networkBtn) {
            setAnnounceLabel(networkBtn, 'Announce');
            networkBtn.disabled = false;
        }
    }
});

function confirmDangerAction(action, onClose) {
    function _close() { if (typeof onClose === 'function') try { onClose(); } catch (_) {} }
    var actions = {
        'clear-paths': {
            msg: 'Clear all cached paths? Paths will be re-discovered over time.',
            command: 'api_clear_paths',
            success: 'Path table cleared.',
            fail: 'Failed to clear paths.'
        },
        'clear-announces': {
            msg: 'Clear announce history?',
            command: 'api_clear_announces',
            success: 'Announce history cleared.',
            fail: 'Failed to clear announce history.'
        },
        'clear-messages': {
            msg: 'Delete ALL messages? This cannot be undone.',
            command: 'api_clear_messages',
            success: 'All messages deleted.',
            fail: 'Failed to delete messages.'
        },
        'clear-contacts': {
            msg: 'Delete ALL contacts? This cannot be undone.',
            command: 'api_clear_contacts',
            success: 'All contacts deleted.',
            fail: 'Failed to delete contacts.'
        },
        'clear-all-data': {
            msg: 'Clear ALL messages and contacts? This cannot be undone.',
            command: 'api_reset_database',
            success: 'All messages and contacts cleared.'
        },
        'factory-reset': null
    };

    if (typeof closeDangerZone === 'function') closeDangerZone();

    if (action === 'factory-reset') {
        rsConfirm({
            message: 'Factory reset?\n\nThis will:\n\u2022 Delete ALL local identities\n\u2022 Delete all contacts and messages\n\u2022 Delete all settings and history\n\u2022 Reset the app to first-run state\n\nHardware identities stored on a YubiKey are not erased. This cannot be undone.',
            danger: true,
            confirmText: 'Delete Everything'
        }).then(function(ok) {
            if (!ok) { _close(); return; }
            return rsConfirm({ message: 'Are you absolutely sure? ALL identities and data will be permanently deleted.', danger: true, confirmText: 'Confirm Factory Reset' });
        }).then(function(ok) {
            if (ok === undefined) return;
            if (!ok) { _close(); return; }
            if (typeof haptic === 'function') haptic('warning');
            showToast('Resetting\u2026', 'toast-orange', 5000);
            RS.invoke('api_factory_reset')
                .then(function() {
                    if (typeof clearFirstRunAnnounceHintDone === 'function') clearFirstRunAnnounceHintDone();
                    // reload() re-requests tauri://localhost/. location.href='/'
                    // breaks on dev-contaminated builds (TAURI_CONFIG leak → dev URL).
                    setTimeout(function() { window.location.reload(); }, 1500);
                })
                .catch(function() {
                    if (typeof haptic === 'function') haptic('error');
                    showToast('Reset failed', 'toast-red', 5000);
                    _close();
                });
        });
        return;
    }

    var cfg = actions[action];
    if (!cfg) return;

    rsConfirm({ message: cfg.msg, danger: true, confirmText: 'Confirm' }).then(function(ok) {
        if (!ok) return;
        RS.invoke(cfg.command).then(function() {
            if (typeof haptic === 'function') haptic('success');
            showToast(cfg.success, '', 3000);
        }).catch(function() {
            if (typeof haptic === 'function') haptic('error');
            showToast(cfg.fail || 'Operation failed', 'toast-red', 3000);
        });
    });
}

var _themeToggleInitialized = false;
var _hapticsToggleInitialized = false;

function initThemeToggle() {
    var toggle = document.getElementById('theme-toggle');
    if (!toggle) return;

    var btns = toggle.querySelectorAll('.theme-toggle-btn');
    var pref = typeof getThemePreference === 'function' ? getThemePreference() : 'auto';

    // Re-sync on every call so view re-entry / identity switch refreshes it.
    btns.forEach(function(btn) {
        btn.classList.toggle('active', btn.getAttribute('data-theme') === pref);
    });

    if (!_themeToggleInitialized) {
        _themeToggleInitialized = true;
        btns.forEach(function(btn) {
            btn.addEventListener('click', function() {
                var theme = this.getAttribute('data-theme');
                if (typeof setTheme === 'function') setTheme(theme);
                btns.forEach(function(b) {
                    b.classList.toggle('active', b.getAttribute('data-theme') === theme);
                });
            });
        });
    }
}

function initHapticsToggle() {
    var toggle = document.getElementById('haptics-enabled-toggle');
    if (!toggle) return;

    toggle.checked = typeof getHapticsEnabled === 'function' ? getHapticsEnabled() : false;

    if (!_hapticsToggleInitialized) {
        _hapticsToggleInitialized = true;
        toggle.addEventListener('change', function() {
            var enabled = !!this.checked;
            if (typeof setHapticsEnabled === 'function') setHapticsEnabled(enabled);
            if (enabled && typeof haptic === 'function') haptic('selection');
        });
    }
}

document.addEventListener('DOMContentLoaded', function() {
    initThemeToggle();
    initHapticsToggle();
    initSettingsSectionNav();
    initAgentSettings();
    renderSettingsVersion();
});

function updateBlockedCount() {
    RS.invoke('api_blocked_contacts').then(function(list) {
        var badge = document.getElementById('settings-blocked-count');
        if (badge) badge.textContent = 'Manage';
    }).catch(function() {});
}

function openBlockListModal() {
    var existing = document.getElementById('block-list-modal-overlay');
    if (existing) {
        if (typeof existing._ratspeakClose === 'function') existing._ratspeakClose();
        else existing.remove();
    }

    var overlay = document.createElement('div');
    overlay.id = 'block-list-modal-overlay';
    overlay.className = 'block-list-overlay';

    var modal = document.createElement('div');
    modal.className = 'block-list-modal';
    modal.innerHTML =
        '<div class="block-list-header">' +
            '<span class="block-list-title">Blocked Users</span>' +
            '<button class="block-list-close" id="block-list-close-btn" aria-label="Close">&times;</button>' +
        '</div>' +
        '<div class="block-list-search-wrap">' +
            '<input type="text" class="block-list-search" id="block-list-search" placeholder="Search blocked users..." autocomplete="off">' +
        '</div>' +
        '<div class="block-list-container" id="block-list-container">' +
            '<div class="loading-state p-12"><span class="loading-spinner"></span> Loading...</div>' +
        '</div>';

    overlay.appendChild(modal);
    document.body.appendChild(overlay);

    var allBlocked = [];

    var refreshFromServer = function() {
        RS.invoke('api_blocked_contacts').then(function(list) {
            if (!document.getElementById('block-list-modal-overlay')) return;
            allBlocked = list;
            var q = document.getElementById('block-list-search');
            renderBlockList(allBlocked, q ? q.value.toLowerCase().trim() : '');
        }).catch(function() {});
    };

    var unlistenPromise = RS.listen('blackhole_update', refreshFromServer);
    var modalClosed = false;
    var escHandler = null;

    function closeModal() {
        if (modalClosed) return;
        modalClosed = true;
        if (escHandler) document.removeEventListener('keydown', escHandler);
        unlistenPromise.then(function(unlisten) { if (typeof unlisten === 'function') unlisten(); });
        overlay.remove();
    }
    overlay._ratspeakClose = closeModal;
    overlay.addEventListener('click', function(e) { if (e.target === overlay) closeModal(); });
    document.getElementById('block-list-close-btn').addEventListener('click', closeModal);
    escHandler = function(e) { if (e.key === 'Escape') closeModal(); };
    document.addEventListener('keydown', escHandler);

    RS.invoke('api_blocked_contacts').then(function(list) {
        allBlocked = list;
        renderBlockList(allBlocked, '');
    }).catch(function() {
        document.getElementById('block-list-container').innerHTML =
            '<div class="block-list-empty">Failed to load block list</div>';
    });

    document.getElementById('block-list-search').addEventListener('input', function() {
        var q = this.value.toLowerCase().trim();
        renderBlockList(allBlocked, q);
    });

    function renderBlockList(list, query) {
        var container = document.getElementById('block-list-container');
        if (!container) return;

        var filtered = list;
        if (query) {
            filtered = list.filter(function(b) {
                return (b.display_name || '').toLowerCase().indexOf(query) !== -1 ||
                       (b.hash || '').toLowerCase().indexOf(query) !== -1;
            });
        }

        if (filtered.length === 0) {
            container.innerHTML = '<div class="block-list-empty">' +
                (query ? 'No matches' : 'No blocked users') + '</div>';
            return;
        }

        var shieldSvg = '<svg class="block-list-shield" viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">' +
            '<path d="M8 1.5 2.5 3.5v4.2c0 3.4 2.3 6.4 5.5 7.3 3.2-.9 5.5-3.9 5.5-7.3V3.5L8 1.5z" ' +
            'fill="currentColor" opacity="0.9"/></svg>';

        var html = '';
        filtered.forEach(function(b) {
            var name = b.display_name || (typeof shortHash === 'function' ? shortHash(b.hash, 8, 4) : b.hash.substring(0, 12) + '\u2026');
            var av = (typeof identityAvatar === 'function') ? identityAvatar(b.hash, 32) : '';
            var dateStr = b.blocked_at ? new Date(b.blocked_at * 1000).toLocaleDateString() : '';
            var shield = b.is_network_blocked
                ? '<span class="block-list-shield-wrap" title="Also dropped at the network layer">' + shieldSvg + '</span>'
                : '';
            // Pending = "Also block on the network" was requested but we have not yet
            // seen this contact's announce, so we cannot resolve their identity hash.
            // The announce-handler escalates on first sighting (Stage 6).
            var pending = b.is_blackhole_pending
                ? '<span class="block-list-pending" title="Network blackhole queued \u2014 will activate on their next announce">pending</span>'
                : '';
            html += '<div class="block-list-row" data-hash="' + escapeHtml(b.hash) +
                    '" data-network-blocked="' + (b.is_network_blocked ? '1' : '0') +
                    '" data-blackhole-pending="' + (b.is_blackhole_pending ? '1' : '0') + '">' +
                '<div class="block-list-row-avatar">' + av + '</div>' +
                '<div class="block-list-row-info">' +
                    '<span class="block-list-row-name">' + escapeHtml(name) + shield + pending + '</span>' +
                    '<span class="block-list-row-meta">' + escapeHtml(typeof shortHash === 'function' ? shortHash(b.hash, 8, 4) : b.hash.substring(0, 16)) + (dateStr ? ' \u00B7 ' + dateStr : '') + '</span>' +
                '</div>' +
            '</div>';
        });
        container.innerHTML = html;

        container.querySelectorAll('.block-list-row').forEach(function(row) {
            row.addEventListener('click', function() {
                var h = this.dataset.hash;
                var isNetworkBlocked = this.dataset.networkBlocked === '1';
                var isPending = this.dataset.blackholePending === '1';
                var entry = list.find(function(b) { return b.hash === h; });
                var displayName = entry ? (entry.display_name || (typeof shortHash === 'function' ? shortHash(h, 8, 4) : h.substring(0, 12))) : (typeof shortHash === 'function' ? shortHash(h, 8, 4) : h.substring(0, 12));

                var afterUnblock = function() {
                    allBlocked = allBlocked.filter(function(b) { return b.hash !== h; });
                    var q = document.getElementById('block-list-search');
                    renderBlockList(allBlocked, q ? q.value.toLowerCase().trim() : '');
                    updateBlockedCount();
                };

                if ((isNetworkBlocked || isPending) && typeof rsConfirmWithCheckbox === 'function') {
                    var help = isPending
                        ? 'Removes the queued network-layer block (it had not yet activated). Uncheck to leave it queued.'
                        : 'Stops dropping their packets at the transport layer. Uncheck to keep the network-level block while restoring contact visibility.';
                    rsConfirmWithCheckbox({
                        message: 'Unblock "' + displayName + '"?',
                        confirmText: 'Unblock',
                        checkboxLabel: 'Also remove the network-layer block',
                        checkboxHelp: help,
                        defaultChecked: true
                    }).then(function(result) {
                        if (!result.confirmed) return;
                        RS.invoke('unblock_contact', { args: { hash: h, also_remove_blackhole: result.checked } }).catch(function() {});
                        afterUnblock();
                    });
                } else {
                    rsConfirm({ message: 'Unblock "' + displayName + '"?', confirmText: 'Unblock' }).then(function(ok) {
                        if (!ok) return;
                        RS.invoke('unblock_contact', { args: { hash: h } }).catch(function() {});
                        afterUnblock();
                    });
                }
            });
        });
    }
}

document.addEventListener('DOMContentLoaded', function() {
    var badge = document.getElementById('settings-blocked-count');
    if (badge) {
        badge.addEventListener('click', openBlockListModal);
    }

    var systemHeaders = document.querySelectorAll(
        '#panel-settings-system .system-subsection-header'
    );
    for (var i = 0; i < systemHeaders.length; i++) {
        systemHeaders[i].addEventListener('click', function() {
            toggleSystemSubsection(this);
        });
        systemHeaders[i].addEventListener('keydown', handleSystemSubsectionKey);
    }
});

RS.listen('contact_blocked', function() { updateBlockedCount(); });
RS.listen('contact_unblocked', function() { updateBlockedCount(); });
// Block-list modal listens for `blackhole_update` itself (line 822) so the
// "pending" pill swaps for the active shield in place when the announce-handler
// promotes a queued entry. Here we only refresh the count badge.
RS.listen('blackhole_promoted', function() { updateBlockedCount(); });
