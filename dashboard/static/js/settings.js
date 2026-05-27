function openSettings() {
    switchView('settings');
    initSettingsSectionNav();
    showSettingsMobileSectionIndex({ restoreFocus: false });
    initHapticsToggle();
    renderSettingsVersion();
    // Re-seal every System Data subsection on each visit. The collapse IS the
    // safety feature for destructive ops — a stale-open Delete Data section
    // from a previous visit would defeat it.
    resetSystemDataCollapse();
}

var _settingsVersionLabel = '';
var _settingsVersionValue = '';
var _settingsUpdateCheckInFlight = false;
var RATSPEAK_RELEASE_LATEST_URL = 'https://api.github.com/repos/ratspeak/Ratspeak/releases/latest';
var RATSPEAK_RELEASES_URL = 'https://github.com/ratspeak/Ratspeak/releases';

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

// Seal all System Data subsections. Called on every Settings open so the
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
    if (typeof updateBlePeerToggle === 'function') updateBlePeerToggle();
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
}

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
    var _notifPanel = document.getElementById('panel-settings-notifications');
    var _notifToggle = document.getElementById('desktop-notifications-toggle');
    if (!_notifPanel || !_notifToggle) return;
    var _isMobile = (typeof isMobile === 'function') ? isMobile() : !!window.__RATSPEAK_MOBILE__;
    if (_isMobile) return;
    _notifPanel.style.display = '';
    if (typeof syncSettingsNavVisibility === 'function') syncSettingsNavVisibility();
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

var _manageIdentitiesBtn = document.getElementById('settings-manage-identities-btn');
if (_manageIdentitiesBtn) {
    _manageIdentitiesBtn.addEventListener('click', function() {
        switchView('identity');
    });
}

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
            message: 'Factory reset?\n\nThis will:\n\u2022 Delete ALL cryptographic identities\n\u2022 Delete all contacts and messages\n\u2022 Delete all settings and history\n\u2022 Reset the app to first-run state\n\nThis cannot be undone.',
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
