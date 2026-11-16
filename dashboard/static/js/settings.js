function openSettings() {
    switchView('settings');
    initSettingsSectionNav();
    showSettingsMobileSectionIndex({ restoreFocus: false });
    initHapticsToggle();
    initHideKnownSpamPeersToggle();
    initPaperMessagesToggle();
    initChannelHostingToggle();
    initActivityIdentityProtectionToggle();
    initDeveloperModeToggle();
    initWindowDecorationsToggle();
    initCloseButtonToggle();
    syncSettingsIdentityActions();
    renderSettingsVersion();
}

var _settingsVersionLabel = '';
var _settingsVersionLine = '';
var _settingsVersionValue = '';
var _settingsUpdateCheckInFlight = false;
var _settingsDeveloperModeBound = false;
var _settingsDeveloperModeStorageKey = 'ratspeak-developer-mode-enabled';
var _settingsDeveloperModeEnabled = readDeveloperModePreference();
var _settingsChannelHostingBound = false;
var _settingsChannelHostingBusy = false;
var _settingsChannelHostingEnabled = false;
var _settingsChannelHostingRequested = null;
var _settingsChannelHostingSupported = null;
var _settingsActivityIdentityProtectionBound = false;
var _settingsActivityIdentityProtectionBusy = false;
var _settingsActivityIdentityProtectionEnabled = true;
var _settingsHideKnownSpamPeersBound = false;
var _settingsHideKnownSpamPeersBusy = false;
var _settingsHideKnownSpamPeersEnabled = true;
var RATSPEAK_RELEASE_LATEST_URL = 'https://api.github.com/repos/ratspeak/Ratspeak/releases/latest';
var RATSPEAK_RELEASES_URL = 'https://github.com/ratspeak/Ratspeak/releases';
var RATSPEAK_TESTFLIGHT_URL = 'https://testflight.apple.com/join/AzzW4Jg6';
var RATSPEAK_RELEASE_TAG_RE = /^v?\d+(\.\d+){1,3}[0-9A-Za-z.-]*$/;

window.ratspeakChannelHostingEnabled = function() {
    return !!_settingsChannelHostingEnabled;
};

// A kit switch: the input carries the state, its label spins while a change is saving.
function _settingsSyncSwitch(input, checked, busy, disabled) {
    if (!input) return;
    input.checked = !!checked;
    input.disabled = !!busy || !!disabled;
    var track = input.closest && input.closest('.ui-switch');
    if (track && track.classList) track.classList.toggle('is-busy', !!busy);
    input.setAttribute('aria-busy', busy ? 'true' : 'false');
}

// A kit segmented control: aria-checked on the chosen button.
function _settingsSyncSeg(group, value, disabled) {
    if (!group) return;
    Array.prototype.forEach.call(group.querySelectorAll('[role="radio"]'), function(btn) {
        btn.setAttribute('aria-checked', btn.getAttribute('data-value') === value ? 'true' : 'false');
        btn.disabled = !!disabled;
    });
}

function _settingsBindSeg(group, onPick) {
    if (!group || group._ratspeakSegBound) return;
    group._ratspeakSegBound = true;
    group.addEventListener('click', function(event) {
        var btn = event.target.closest && event.target.closest('[role="radio"]');
        if (!btn || btn.disabled || btn.getAttribute('aria-checked') === 'true') return;
        onPick(btn.getAttribute('data-value'));
    });
    group.addEventListener('keydown', function(event) {
        if (['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].indexOf(event.key) === -1) return;
        var buttons = Array.prototype.slice.call(group.querySelectorAll('[role="radio"]:not(:disabled)'));
        var at = buttons.indexOf(document.activeElement);
        if (at === -1) return;
        event.preventDefault();
        var step = event.key === 'ArrowLeft' || event.key === 'ArrowUp' ? -1 : 1;
        var next = buttons[(at + step + buttons.length) % buttons.length];
        next.focus();
        onPick(next.getAttribute('data-value'));
    });
}

function syncChannelHostingToggleState() {
    var toggle = document.getElementById('settings-channel-hosting-toggle');
    var desc = document.getElementById('settings-channel-hosting-desc');
    var displayedEnabled = _settingsChannelHostingRequested === null
        ? _settingsChannelHostingEnabled
        : _settingsChannelHostingRequested;
    if (document.documentElement) {
        document.documentElement.dataset.channelHosting = displayedEnabled ? 'on' : 'off';
    }
    _settingsSyncSwitch(toggle, displayedEnabled, _settingsChannelHostingBusy,
        _settingsChannelHostingSupported === false && !displayedEnabled);
    if (desc) {
        if (_settingsChannelHostingBusy) {
            desc.textContent = _settingsChannelHostingRequested
                ? 'Turning on channel hosting…'
                : 'Stopping your hub…';
        } else {
            desc.textContent = _settingsChannelHostingSupported === false
                ? 'Channel hosting is available in the desktop app.'
                : 'Lets this device host channels.';
        }
    }
}

function adoptChannelHostingFromBackend(enabled, supported) {
    _settingsChannelHostingEnabled = !!enabled;
    if (supported !== undefined) _settingsChannelHostingSupported = !!supported;
    if (typeof channelHubOverview !== 'undefined' && channelHubOverview) {
        channelHubOverview.hosting_enabled = _settingsChannelHostingEnabled;
    }
    syncChannelHostingToggleState();
    if (typeof channelHubRenderHome === 'function') channelHubRenderHome(channelHubOverview);
}

function setChannelHostingEnabled(enabled) {
    if (_settingsChannelHostingBusy) return;
    if (enabled && _settingsChannelHostingSupported === false) return;
    _settingsChannelHostingRequested = !!enabled;
    _settingsChannelHostingBusy = true;
    syncChannelHostingToggleState();

    RS.invoke('set_channel_hosting_enabled', { enabled: !!enabled }).then(function(overview) {
        if (overview && typeof _channelHubApplyOverview === 'function') {
            _channelHubApplyOverview(overview);
        }
        adoptChannelHostingFromBackend(
            overview && overview.hosting_enabled !== undefined
                ? overview.hosting_enabled
                : enabled,
            overview ? overview.supported : undefined
        );
    }).catch(function(error) {
        if (typeof showToast === 'function') {
            showToast((error && error.message) || 'Could not update channel hosting', 'toast-error', 3200);
        }
        return Promise.all([
            RS.invoke('api_app_settings').then(applyAppSettingsPayload).catch(function() {}),
            RS.invoke('api_channel_hub').then(function(overview) {
                if (!overview) return;
                if (typeof _channelHubApplyOverview === 'function') {
                    _channelHubApplyOverview(overview);
                }
                adoptChannelHostingFromBackend(overview.hosting_enabled, overview.supported);
            }).catch(function() {})
        ]);
    }).then(function() {
        _settingsChannelHostingRequested = null;
        _settingsChannelHostingBusy = false;
        syncChannelHostingToggleState();
    });
}

function initChannelHostingToggle() {
    var toggle = document.getElementById('settings-channel-hosting-toggle');
    if (!toggle) return;
    syncChannelHostingToggleState();
    if (!_settingsChannelHostingBound) {
        _settingsChannelHostingBound = true;
        toggle.addEventListener('change', function() {
            setChannelHostingEnabled(!!toggle.checked);
        });
    }
    if (typeof channelHubLoad === 'function') {
        channelHubLoad(false).then(function(overview) {
            if (!overview) return;
            adoptChannelHostingFromBackend(overview.hosting_enabled, overview.supported);
        }).catch(function() {});
    }
}

function syncActivityIdentityProtectionToggleState() {
    _settingsSyncSwitch(document.getElementById('settings-activity-identity-protection-toggle'),
        _settingsActivityIdentityProtectionEnabled, _settingsActivityIdentityProtectionBusy);
}

function adoptActivityIdentityProtectionFromBackend(enabled) {
    _settingsActivityIdentityProtectionEnabled = enabled !== false;
    syncActivityIdentityProtectionToggleState();
    if (RS.activityIdentityProtection && typeof RS.activityIdentityProtection.set === 'function') {
        RS.activityIdentityProtection.set(_settingsActivityIdentityProtectionEnabled);
    }
}

function saveActivityIdentityProtectionEnabled(enabled) {
    if (_settingsActivityIdentityProtectionBusy) return;
    var previous = _settingsActivityIdentityProtectionEnabled;
    _settingsActivityIdentityProtectionEnabled = !!enabled;
    _settingsActivityIdentityProtectionBusy = true;
    syncActivityIdentityProtectionToggleState();
    if (RS.activityIdentityProtection && typeof RS.activityIdentityProtection.set === 'function') {
        RS.activityIdentityProtection.set(_settingsActivityIdentityProtectionEnabled);
    }
    RS.invoke('set_activity_identity_protection', { enabled: _settingsActivityIdentityProtectionEnabled })
        .then(function(result) {
            adoptActivityIdentityProtectionFromBackend(
                result && result.enabled !== undefined ? result.enabled : enabled
            );
        })
        .catch(function(error) {
            adoptActivityIdentityProtectionFromBackend(previous);
            if (typeof showToast === 'function') {
                showToast((error && error.message) || 'Could not update Activity privacy', 'toast-error', 4000);
            }
        })
        .then(function() {
            _settingsActivityIdentityProtectionBusy = false;
            syncActivityIdentityProtectionToggleState();
        });
}

function initActivityIdentityProtectionToggle() {
    var toggle = document.getElementById('settings-activity-identity-protection-toggle');
    if (!toggle) return;
    syncActivityIdentityProtectionToggleState();
    if (_settingsActivityIdentityProtectionBound) return;
    _settingsActivityIdentityProtectionBound = true;
    toggle.addEventListener('change', function() {
        saveActivityIdentityProtectionEnabled(!!toggle.checked);
    });
}

function syncHideKnownSpamPeersToggleState() {
    _settingsSyncSwitch(document.getElementById('settings-hide-known-spam-peers-toggle'),
        _settingsHideKnownSpamPeersEnabled, _settingsHideKnownSpamPeersBusy);
}

function adoptHideKnownSpamPeersFromBackend(enabled) {
    _settingsHideKnownSpamPeersEnabled = enabled !== false;
    syncHideKnownSpamPeersToggleState();
    if (typeof PeersCache !== 'undefined' && PeersCache &&
        typeof PeersCache.setHideKnownSpamPeers === 'function') {
        PeersCache.setHideKnownSpamPeers(_settingsHideKnownSpamPeersEnabled);
    }
    // Home and Peers both read PeersCache.enriched(); repaint Home now so a
    // preference change is visible without waiting for the next stats tick.
    if (typeof renderDashboardPeersList === 'function') renderDashboardPeersList();
    if (typeof scheduleRenderPeersList === 'function') {
        if (typeof _peersLastDirtyKey !== 'undefined') _peersLastDirtyKey = '';
        scheduleRenderPeersList();
    }
}

function setHideKnownSpamPeersEnabled(enabled) {
    if (_settingsHideKnownSpamPeersBusy) return;
    var previous = _settingsHideKnownSpamPeersEnabled;
    _settingsHideKnownSpamPeersEnabled = !!enabled;
    _settingsHideKnownSpamPeersBusy = true;
    adoptHideKnownSpamPeersFromBackend(_settingsHideKnownSpamPeersEnabled);
    syncHideKnownSpamPeersToggleState();
    RS.invoke('set_hide_known_spam_peers', { enabled: _settingsHideKnownSpamPeersEnabled })
        .then(function(result) {
            adoptHideKnownSpamPeersFromBackend(
                result && result.enabled !== undefined ? result.enabled : enabled
            );
        })
        .catch(function(error) {
            adoptHideKnownSpamPeersFromBackend(previous);
            if (typeof showToast === 'function') {
                showToast((error && error.message) || 'Could not update peer visibility', 'toast-error', 4000);
            }
        })
        .then(function() {
            _settingsHideKnownSpamPeersBusy = false;
            syncHideKnownSpamPeersToggleState();
        });
}

function initHideKnownSpamPeersToggle() {
    var toggle = document.getElementById('settings-hide-known-spam-peers-toggle');
    if (!toggle) return;
    syncHideKnownSpamPeersToggleState();
    if (_settingsHideKnownSpamPeersBound) return;
    _settingsHideKnownSpamPeersBound = true;
    toggle.addEventListener('change', function() {
        setHideKnownSpamPeersEnabled(!!toggle.checked);
    });
}

window.ratspeakHideKnownSpamPeersEnabled = function() {
    return !!_settingsHideKnownSpamPeersEnabled;
};

var _settingsPaperMessagesBound = false;
var _settingsPaperMessagesBusy = false;
var _settingsPaperMessagesEnabled = false;

// Gates only the manual decoder entry points; sending, scanning, and links always work.
function syncPaperMessagesToggleState() {
    if (document.documentElement) {
        document.documentElement.dataset.paperMessages = _settingsPaperMessagesEnabled ? 'on' : 'off';
    }
    _settingsSyncSwitch(document.getElementById('settings-paper-messages-toggle'),
        _settingsPaperMessagesEnabled, _settingsPaperMessagesBusy);
}

function adoptPaperMessagesFromBackend(enabled) {
    _settingsPaperMessagesEnabled = enabled === true;
    syncPaperMessagesToggleState();
}

function setPaperMessagesEnabled(enabled) {
    if (_settingsPaperMessagesBusy) return;
    var previous = _settingsPaperMessagesEnabled;
    _settingsPaperMessagesEnabled = !!enabled;
    _settingsPaperMessagesBusy = true;
    syncPaperMessagesToggleState();
    RS.invoke('set_paper_messages_enabled', { enabled: _settingsPaperMessagesEnabled })
        .then(function(result) {
            var next = result && result.enabled !== undefined ? result.enabled
                : (result && result.paper_messages_enabled !== undefined ? result.paper_messages_enabled : enabled);
            adoptPaperMessagesFromBackend(!!next);
        })
        .catch(function(error) {
            adoptPaperMessagesFromBackend(previous);
            if (typeof showToast === 'function') {
                showToast((error && error.message) || 'Could not update paper messages', 'toast-error', 4000);
            }
        })
        .then(function() {
            _settingsPaperMessagesBusy = false;
            syncPaperMessagesToggleState();
        });
}

function initPaperMessagesToggle() {
    var toggle = document.getElementById('settings-paper-messages-toggle');
    syncPaperMessagesToggleState();
    if (!toggle || _settingsPaperMessagesBound) return;
    _settingsPaperMessagesBound = true;
    toggle.addEventListener('change', function() {
        setPaperMessagesEnabled(!!toggle.checked);
    });
}

window.ratspeakPaperMessagesEnabled = function() {
    return !!_settingsPaperMessagesEnabled;
};

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
    syncDeveloperModeToggleState();
    notifyDeveloperModeChanged();
    // SQLite is the durable store; localStorage is only a boot-time cache
    // (WKWebView drops custom-scheme localStorage on macOS/iOS).
    RS.invoke('set_developer_mode', { enabled: _settingsDeveloperModeEnabled }).catch(function() {});
}

// Backend value wins over the localStorage cache at boot; no write-back.
function adoptDeveloperModeFromBackend(enabled) {
    enabled = !!enabled;
    if (enabled === _settingsDeveloperModeEnabled) return;
    _settingsDeveloperModeEnabled = enabled;
    persistDeveloperModePreference(enabled);
    syncDeveloperModeToggleState();
    notifyDeveloperModeChanged();
}

window.ratspeakDeveloperModeEnabled = function() {
    return !!_settingsDeveloperModeEnabled;
};

function syncDeveloperModeToggleState() {
    var toggle = document.getElementById('settings-developer-mode-toggle');
    if (toggle) toggle.checked = _settingsDeveloperModeEnabled;
}

function initDeveloperModeToggle() {
    var toggle = document.getElementById('settings-developer-mode-toggle');
    if (!toggle) return;
    syncDeveloperModeToggleState();
    if (_settingsDeveloperModeBound) return;
    _settingsDeveloperModeBound = true;
    toggle.addEventListener('change', function() {
        setDeveloperModeEnabled(!!toggle.checked);
    });
}

var _settingsWindowDecorationsBound = false;
var _settingsWindowDecorationsMode = 'auto';

// Linux desktop only: GTK is the only backend that draws its own title bar
// (tao WlHeader on Wayland); macOS/Windows decorations are native.
function windowDecorationsRowApplies() {
    return !!window.__RATSPEAK_DESKTOP__ && /linux/i.test(navigator.userAgent);
}

// The Window group shows when one of its rows applies on this platform.
function syncSettingsWindowBlock() {
    var block = document.getElementById('settings-window-block');
    if (!block) return;
    var decorations = windowDecorationsRowApplies();
    var close = closeButtonRowApplies();
    var notice = document.getElementById('settings-window-decorations-notice');
    if (notice) notice.hidden = !decorations;
    block.hidden = !(decorations || close);
}

function syncWindowDecorationsToggleState() {
    _settingsSyncSeg(document.getElementById('settings-window-decorations-seg'), _settingsWindowDecorationsMode);
}

function setWindowDecorationsMode(mode) {
    if (['auto', 'on', 'off'].indexOf(mode) === -1) return;
    _settingsWindowDecorationsMode = mode;
    syncWindowDecorationsToggleState();
    RS.invoke('set_window_decorations', { mode: mode }).then(function(result) {
        if (result && result.restart_required) {
            showToast('Restart Ratspeak to apply the title bar change', 'toast-info', 4000);
        }
    }).catch(function() {
        showToast('Could not update window decorations', 'toast-error', 4000);
    });
}

function adoptWindowDecorationsFromBackend(mode) {
    if (['auto', 'on', 'off'].indexOf(mode) === -1) mode = 'auto';
    if (mode === _settingsWindowDecorationsMode) return;
    _settingsWindowDecorationsMode = mode;
    syncWindowDecorationsToggleState();
}

function initWindowDecorationsToggle() {
    var row = document.getElementById('settings-window-decorations-row');
    syncSettingsWindowBlock();
    if (!row || !windowDecorationsRowApplies()) return;
    row.hidden = false;
    syncWindowDecorationsToggleState();
    if (_settingsWindowDecorationsBound) return;
    _settingsWindowDecorationsBound = true;
    _settingsBindSeg(document.getElementById('settings-window-decorations-seg'), setWindowDecorationsMode);
}

var _settingsCloseButtonBound = false;
var _settingsCloseButtonAction = 'ask';
var _CLOSE_BUTTON_ACTIONS = ['ask', 'tray', 'quit'];

// Windows only: the tray menu always quits, so only the close button needs a choice.
function closeButtonRowApplies() {
    return !!window.__RATSPEAK_DESKTOP__ && /windows/i.test(navigator.userAgent);
}

function syncCloseButtonToggleState() {
    _settingsSyncSeg(document.getElementById('settings-close-button-seg'), _settingsCloseButtonAction);
}

function setCloseButtonAction(action) {
    if (_CLOSE_BUTTON_ACTIONS.indexOf(action) === -1) return;
    var previous = _settingsCloseButtonAction;
    _settingsCloseButtonAction = action;
    syncCloseButtonToggleState();
    RS.invoke('set_close_button_action', { mode: action }).catch(function() {
        _settingsCloseButtonAction = previous;
        syncCloseButtonToggleState();
        showToast('Could not update the close button', 'toast-error', 4000);
    });
}

function adoptCloseButtonActionFromBackend(action) {
    if (_CLOSE_BUTTON_ACTIONS.indexOf(action) === -1) action = 'ask';
    _settingsCloseButtonAction = action;
    syncCloseButtonToggleState();
}

function initCloseButtonToggle() {
    var row = document.getElementById('settings-close-button-row');
    syncSettingsWindowBlock();
    if (!row || !closeButtonRowApplies()) return;
    row.hidden = false;
    syncCloseButtonToggleState();
    if (_settingsCloseButtonBound) return;
    _settingsCloseButtonBound = true;
    _settingsBindSeg(document.getElementById('settings-close-button-seg'), setCloseButtonAction);
}

// About: the version, and one update row. By default the row says when Ratspeak
// last checked; after a check it shows only the result until About is left.
var _settingsUpdateState = 'idle';
var _settingsUpdateRelease = null;
var _settingsUpdateReason = '';
var _settingsLastChecked = 0;

function renderSettingsVersion() {
    var versionEl = document.getElementById('settings-about-version');
    if (_settingsVersionLabel) {
        if (versionEl) versionEl.textContent = _settingsVersionLine;
        renderSettingsUpdateRow();
        return;
    }
    renderSettingsUpdateRow();
    RS.invoke('api_version').then(function(data) {
        var name = (data && data.name) || 'Ratspeak';
        var version = (data && data.version) || '';
        if (!version) return;
        _settingsVersionValue = version;
        _settingsVersionLabel = name + ' ' + version;
        // The protocol releases the bundled stack follows, once the backend reports them.
        _settingsVersionLine = ['Version ' + version, data.reticulum ? 'Reticulum ' + data.reticulum : '', data.lxmf ? 'LXMF ' + data.lxmf : '']
            .filter(Boolean).join(' · ');
        if (versionEl) versionEl.textContent = _settingsVersionLine;
        renderSettingsUpdateRow();
    }).catch(function() {});
}

function _settingsLastCheckedText() {
    if (!_settingsLastChecked) return 'Last checked: never';
    var full = RS.kit && RS.kit.time ? RS.kit.time.full(_settingsLastChecked) : new Date(_settingsLastChecked * 1000).toLocaleString();
    return 'Last checked: ' + full;
}

function renderSettingsUpdateRow() {
    var label = document.getElementById('settings-update-label');
    var btn = document.getElementById('settings-update-btn');
    if (!label || !btn) return;
    var state = _settingsUpdateState;
    btn.hidden = state === 'current';
    btn.disabled = state === 'checking';
    btn.removeAttribute('aria-busy');
    btn.className = 'ui-btn ui-btn--sm' + (state === 'update' ? ' ui-btn--primary' : '');
    label.title = state === 'failed' ? _settingsUpdateReason : '';
    if (state === 'current') {
        label.textContent = 'You’re up to date.';
    } else if (state === 'update' && _settingsUpdateRelease) {
        label.textContent = 'Version ' + _settingsUpdateRelease.version + ' is available';
        btn.textContent = 'Update';
    } else if (state === 'failed') {
        label.textContent = 'Couldn’t check for updates';
        btn.textContent = 'Try again';
    } else {
        label.textContent = _settingsLastCheckedText();
        if (state === 'checking') {
            btn.setAttribute('aria-busy', 'true');
            btn.innerHTML = '<span class="ui-spin" aria-hidden="true"></span>Checking…';
        } else {
            btn.textContent = 'Check for updates';
        }
    }
}

function _settingsSetUpdateState(state) {
    _settingsUpdateState = state;
    _settingsUpdateCheckInFlight = state === 'checking';
    renderSettingsUpdateRow();
}

// Leaving About forgets the last result; coming back shows when it last checked.
function _settingsResetUpdateRow() {
    if (_settingsUpdateState === 'checking') return;
    _settingsUpdateRelease = null;
    _settingsSetUpdateState('idle');
}

function adoptUpdateLastCheckedFromBackend(at) {
    var value = Number(at) || 0;
    if (value > _settingsLastChecked) _settingsLastChecked = value;
    renderSettingsUpdateRow();
}

function _settingsRecordUpdateCheck() {
    _settingsLastChecked = Math.floor(Date.now() / 1000);
    RS.invoke('set_ui_state', { key: 'update_last_checked', value: _settingsLastChecked }).catch(function() {});
}

(function() {
    var btn = document.getElementById('settings-update-btn');
    if (!btn) return;
    btn.addEventListener('click', function() {
        if (_settingsUpdateState === 'update' && _settingsUpdateRelease) {
            _settingsShowUpdateAvailable(_settingsUpdateRelease, _settingsNormalizeVersion(_settingsVersionValue));
            return;
        }
        promptRatspeakUpdateCheck(_settingsVersionValue);
    });
})();

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

// The busy state disables (and so blurs) the button, so dialogs can't restore focus to it themselves.
function _settingsFocusUpdateButton() {
    if (RS.ui && typeof RS.ui.prefersKeyboardFocus === 'function' && !RS.ui.prefersKeyboardFocus()) return;
    var btn = document.getElementById('settings-update-btn');
    if (btn && !btn.disabled && !btn.hidden) btn.focus();
}

function _settingsUpdateError(kind, status) {
    var error = new Error(kind === 'http' ? 'GitHub returned ' + status : 'Unexpected release response');
    error.kind = kind;
    error.status = status;
    return error;
}

function _settingsLatestRelease(signal) {
    return fetch(RATSPEAK_RELEASE_LATEST_URL, {
        method: 'GET',
        cache: 'no-store',
        signal: signal,
        headers: {
            'Accept': 'application/vnd.github+json'
        }
    }).then(function(resp) {
        if (!resp.ok) throw _settingsUpdateError('http', resp.status);
        return resp.json().catch(function() { throw _settingsUpdateError('bad-response'); });
    }).then(function(data) {
        var latest = _settingsNormalizeVersion(data && (data.tag_name || data.name));
        if (!latest) throw _settingsUpdateError('bad-response');
        return {
            version: latest,
            tag: data && typeof data.tag_name === 'string' ? data.tag_name : '',
            publishedAt: data && typeof data.published_at === 'string' ? data.published_at : '',
            body: data && typeof data.body === 'string' ? data.body : '',
        };
    });
}

// Release notes arrive as Markdown; drop a leading heading that only repeats the version.
function _settingsReleaseNotesBody(body, latestVersion) {
    var text = String(body || '').replace(/\r\n?/g, '\n');
    var lines = text.split('\n');
    var i = 0;
    while (i < lines.length && !lines[i].trim()) i++;
    var heading = i < lines.length ? /^#{1,6}[ \t]+(.+?)[ \t#]*$/.exec(lines[i]) : null;
    if (heading && _settingsNormalizeVersion(heading[1]) === latestVersion) {
        i++;
        while (i < lines.length && !lines[i].trim()) i++;
        text = lines.slice(i).join('\n');
    }
    return text.trim();
}

function _settingsReleaseUrl(tag) {
    return RATSPEAK_RELEASE_TAG_RE.test(tag || '')
        ? RATSPEAK_RELEASES_URL + '/tag/' + encodeURIComponent(tag)
        : RATSPEAK_RELEASES_URL;
}

function _settingsElement(tag, className, text) {
    var el = document.createElement(tag);
    if (className) el.className = className;
    if (text != null) el.textContent = text;
    return el;
}

// The notes are untrusted remote text: they reach the page only through the
// escaping Markdown renderer, and their links get the leaving-app warning.
function _settingsRenderReleaseNotes(notes, body, ios) {
    if (!body) {
        notes.appendChild(_settingsElement('p', 'rs-update-notes-empty', 'No release notes for this version.'));
        return;
    }
    if (!(window.RS && RS.markdown)) {
        notes.classList.add('rs-release-notes-plain');
        notes.textContent = body;
        return;
    }
    var html = RS.markdown.render(body, { links: typeof _messageLinkRanges === 'function' ? _messageLinkRanges : null });
    if (html == null) {
        notes.appendChild(_settingsElement('p', 'rs-update-notes-empty', ios
            ? 'These notes are too long to show here.'
            : 'These notes are too long to show here. Get Update opens them on GitHub.'));
        return;
    }
    notes.innerHTML = html;
    notes.querySelectorAll('.md-spoiler').forEach(function(spoiler) {
        if (typeof _revealMessageSpoiler === 'function') _revealMessageSpoiler(spoiler, true);
    });
    notes.addEventListener('click', function(e) {
        var link = e.target.closest('.rs-message-link[data-url]');
        if (link) {
            e.preventDefault();
            e.stopPropagation();
            if (typeof _confirmExternalLink === 'function') _confirmExternalLink(link.getAttribute('data-url'), link);
            return;
        }
        var copy = e.target.closest('.md-code-copy');
        if (!copy) return;
        e.preventDefault();
        var pre = copy.closest('.md-codeblock') && copy.closest('.md-codeblock').querySelector('.md-pre');
        Promise.resolve(RS.copyText(pre ? pre.textContent : '')).then(function() {
            showToast('Code copied', 'toast-success', 1800);
        }).catch(function() {
            showToast('Could not copy', 'toast-error', 1800);
        });
    });
    notes.addEventListener('mousedown', function(e) {
        if (e.button === 1 && e.target.closest('.rs-message-link')) e.preventDefault();
    });
    notes.addEventListener('auxclick', function(e) {
        var link = e.button === 1 && e.target.closest('.rs-message-link[data-url]');
        if (!link) return;
        e.preventDefault();
        if (typeof _confirmExternalLink === 'function') _confirmExternalLink(link.getAttribute('data-url'), link);
    });
}

function _settingsShowUpdateAvailable(release, currentVersion) {
    return new Promise(function(resolve) {
        var ios = typeof isIOS === 'function' && isIOS();
        var observer = null;
        var built = _rsBuildSheet({
            title: 'Update available',
            ariaLabel: 'Update available',
            size: 'md',
            sheetClass: 'rs-update-sheet',
            nativeBackValue: null,
        }, function() {
            if (observer) observer.disconnect();
            _settingsFocusUpdateButton();
            resolve();
        });
        built.sheet.tabIndex = -1;
        built.overlay.addEventListener('click', function(e) {
            if (e.target === built.overlay) built.dismiss(null);
        });
        built.sheet.setAttribute('aria-describedby', 'rs-update-summary');

        var summary = _settingsElement('div', 'rs-update-summary');
        summary.id = 'rs-update-summary';
        summary.tabIndex = -1;
        summary.appendChild(_settingsElement('div', 'rs-update-version', 'Ratspeak ' + release.version));
        var meta = _settingsElement('div', 'rs-update-meta');
        var published = release.publishedAt ? new Date(release.publishedAt) : null;
        if (published && !isNaN(published.getTime())) {
            var time = _settingsElement('time', null,
                'Released ' + published.toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' }));
            time.setAttribute('datetime', release.publishedAt);
            meta.appendChild(time);
            meta.appendChild(document.createTextNode(' · '));
        }
        meta.appendChild(document.createTextNode("You're on " + currentVersion));
        summary.appendChild(meta);
        built.body.appendChild(summary);

        var section = _settingsElement('section', 'rs-update-notes');
        section.setAttribute('aria-labelledby', 'rs-update-notes-label');
        var label = _settingsElement('h2', 'rs-update-notes-label', "What's new");
        label.id = 'rs-update-notes-label';
        var notes = _settingsElement('div', 'rs-release-notes md');
        section.appendChild(label);
        section.appendChild(notes);
        built.body.appendChild(section);
        _settingsRenderReleaseNotes(notes, _settingsReleaseNotesBody(release.body, release.version), ios);

        built.footer.appendChild(_settingsElement('p', 'rs-update-footnote', ios
            ? "For privacy, Ratspeak doesn't update itself. On iPhone and iPad, updates come through TestFlight."
            : "For privacy, Ratspeak doesn't update itself. Get Update opens the release on GitHub."));
        var closeBtn = _settingsElement('button', 'rs-dialog-cancel', 'Close');
        closeBtn.type = 'button';
        closeBtn.addEventListener('click', function() { built.dismiss(null); });
        var getBtn = _settingsElement('button', 'rs-dialog-confirm', ios ? 'Open TestFlight' : 'Get Update');
        getBtn.type = 'button';
        getBtn.addEventListener('click', function() {
            var failure = ios ? 'Could not open TestFlight' : 'Could not open GitHub';
            RS.openExternalUrl(ios ? RATSPEAK_TESTFLIGHT_URL : _settingsReleaseUrl(release.tag)).then(function(opened) {
                if (opened) built.dismiss(null);
                else showToast(failure, 'toast-error', 3500);
            }).catch(function() {
                showToast(failure, 'toast-error', 3500);
            });
        });
        built.footer.appendChild(closeBtn);
        built.footer.appendChild(getBtn);

        // The body is the only scroller; it becomes a labelled tab stop only when it scrolls.
        function syncScroller() {
            var body = built.body;
            if (body.scrollHeight > body.clientHeight + 1) {
                body.setAttribute('tabindex', '0');
                body.setAttribute('role', 'region');
                body.setAttribute('aria-label', "What's new in " + release.version);
            } else {
                body.removeAttribute('tabindex');
                body.removeAttribute('role');
                body.removeAttribute('aria-label');
            }
            notes.querySelectorAll('.md-pre').forEach(function(pre) {
                if (typeof _syncCodeBlockFocus === 'function') _syncCodeBlockFocus(pre);
            });
        }

        built.sheet.addEventListener('keydown', function(e) {
            if (e.key === 'Escape') {
                e.stopPropagation();
                built.dismiss(null);
                return;
            }
            if (e.key !== 'Tab') return;
            var focusable = Array.prototype.filter.call(
                built.sheet.querySelectorAll('a[href], button:not(:disabled), [tabindex="0"]'),
                function(el) { return el.offsetParent !== null || el === built.body; }
            );
            if (!focusable.length) return;
            var first = focusable[0];
            var last = focusable[focusable.length - 1];
            if (e.shiftKey && (document.activeElement === first || document.activeElement === summary)) {
                e.preventDefault();
                last.focus();
            } else if (!e.shiftKey && document.activeElement === last) {
                e.preventDefault();
                first.focus();
            }
        });

        RS.gestures.attachDragDismiss(built.sheet, {
            axis: 'y',
            blockIfScrolled: true,
            skipIf: function(e) {
                return !!(e.target.closest('button') || e.target.closest('a') || e.target.closest('.md-pre'));
            },
            parallaxOverlay: built.overlay,
            onCommit: function() { built.dismiss(null); }
        });

        built.present();
        syncScroller();
        if (typeof ResizeObserver === 'function') {
            observer = new ResizeObserver(syncScroller);
            observer.observe(built.body);
            observer.observe(notes);
        }
        summary.focus({ preventScroll: true });
    });
}

function _settingsUpdateFailureReason(err) {
    if (err && err.kind === 'bad-response') return 'GitHub sent an unexpected response. Try again later.';
    if (err && err.kind === 'http' && (err.status === 403 || err.status === 429)) {
        return 'GitHub is limiting requests right now. Try again in a few minutes.';
    }
    return 'Check your internet connection and try again.';
}

function checkRatspeakUpdate(currentVersion) {
    currentVersion = _settingsNormalizeVersion(currentVersion || _settingsVersionValue);
    if (!currentVersion) {
        _settingsUpdateReason = "Ratspeak couldn't read its own version. Restart the app and try again.";
        _settingsSetUpdateState('failed');
        return Promise.resolve();
    }

    _settingsSetUpdateState('checking');
    var controller = typeof AbortController !== 'undefined' ? new AbortController() : null;
    var timeoutId = setTimeout(function() {
        if (controller) controller.abort();
    }, 12000);

    return _settingsLatestRelease(controller ? controller.signal : undefined).then(function(release) {
        clearTimeout(timeoutId);
        _settingsRecordUpdateCheck();
        if (_settingsCompareVersions(release.version, currentVersion) > 0) {
            _settingsUpdateRelease = release;
            _settingsSetUpdateState('update');
        } else {
            // "You're up to date": the row says it in place of a dialog.
            _settingsSetUpdateState('current');
        }
        _settingsFocusUpdateButton();
    }, function(err) {
        clearTimeout(timeoutId);
        window.RS.diag('warn', '[settings] update check failed:', err);
        _settingsUpdateReason = _settingsUpdateFailureReason(err);
        _settingsSetUpdateState('failed');
        _settingsFocusUpdateButton();
    });
}

function promptRatspeakUpdateCheck(currentVersion) {
    if (_settingsUpdateCheckInFlight) return;
    rsConfirm({
        title: 'Check for updates?',
        message: 'Checking for the latest version requires an internet connection and will compare your version with the current available version.',
        confirmText: 'Continue',
        cancelText: 'Cancel'
    }).then(function(ok) {
        if (!ok) return;
        return checkRatspeakUpdate(currentVersion);
    });
}

var SETTINGS_DEFAULT_PANEL_ID = 'panel-settings-general';
var _settingsSectionNavBound = false;
var _settingsSectionResizeBound = false;
var _settingsMobileBackBound = false;
var _settingsMobileDetailActive = false;
var _settingsShownPanelId = '';

function _settingsDetailModeActive() {
    return !!(window.matchMedia && window.matchMedia('(min-width: 769px)').matches);
}

function _settingsMobileModeActive() {
    return !!(window.matchMedia && window.matchMedia('(max-width: 768px)').matches);
}

function _settingsPanelAvailable(panel) {
    return !!(panel && !panel.hidden && panel.style.display !== 'none');
}

function _settingsNavItems() {
    return document.querySelectorAll('#view-settings .settings-nav-item[data-settings-panel]');
}

function _settingsFirstAvailablePanelId() {
    var items = document.querySelectorAll('#settings-section-nav [data-settings-panel]');
    for (var i = 0; i < items.length; i++) {
        var panelId = items[i].dataset.settingsPanel;
        if (_settingsPanelAvailable(document.getElementById(panelId))) return panelId;
    }
    return SETTINGS_DEFAULT_PANEL_ID;
}

function syncSettingsNavVisibility() {
    var activeHidden = false;
    _settingsNavItems().forEach(function(item) {
        var available = _settingsPanelAvailable(document.getElementById(item.dataset.settingsPanel));
        item.hidden = !available;
        if (!available && item.getAttribute('aria-current') === 'page') activeHidden = true;
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

    var changed = _settingsShownPanelId !== panelId;
    if (changed && panelId === 'panel-settings-about') _settingsResetUpdateRow();
    _settingsShownPanelId = panelId;

    document.querySelectorAll('#view-settings .settings-panel').forEach(function(el) {
        el.classList.toggle('settings-panel-selected', el.id === panelId);
    });

    var activeItem = null;
    document.querySelectorAll('#settings-section-nav [data-settings-panel]').forEach(function(item) {
        var selected = item.dataset.settingsPanel === panelId;
        if (selected) {
            item.setAttribute('aria-current', 'page');
            activeItem = item;
        } else {
            item.removeAttribute('aria-current');
        }
    });

    var head = document.getElementById('settings-head');
    var title = document.getElementById('settings-detail-title');
    var desc = document.getElementById('settings-detail-desc');
    var mobileTitle = document.getElementById('settings-mobile-detail-title');
    if (activeItem) {
        var settingsTitle = activeItem.dataset.settingsTitle || activeItem.textContent.trim();
        var settingsDesc = activeItem.dataset.settingsDesc || '';
        if (title) title.textContent = settingsTitle;
        if (mobileTitle) mobileTitle.textContent = settingsTitle;
        if (desc) {
            desc.textContent = settingsDesc;
            desc.hidden = !settingsDesc;
        }
        if (head) head.classList.toggle('is-bare', !settingsDesc);
    }

    if (changed) {
        var pane = document.querySelector('#view-settings .settings-detail-pane');
        if (pane) pane.scrollTop = 0;
    }

    if (!opts.skipStore) {
        try { localStorage.setItem('ratspeak_settings_section', panelId); } catch(e) {}
    }

    if (opts.showMobileDetail) _settingsMobileDetailActive = true;
    syncSettingsMobileLayout();
}

function initSettingsSectionNav() {
    var view = document.getElementById('view-settings');
    if (!view) return;

    if (!_settingsSectionNavBound) {
        _settingsNavItems().forEach(function(item) {
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

    // Only width selects the section layout; entering Settings re-syncs it.
    if (!_settingsSectionResizeBound) {
        watchViewLayout('settings', function() {
            var active = document.querySelector('#settings-section-nav [aria-current="page"]');
            selectSettingsSection(active ? active.dataset.settingsPanel : _settingsFirstAvailablePanelId(), { skipStore: true });
        });
        _settingsSectionResizeBound = true;
    }

    syncSettingsNavVisibility();
    var selected = SETTINGS_DEFAULT_PANEL_ID;
    try {
        selected = localStorage.getItem('ratspeak_settings_section') || selected;
    } catch(e) {}
    // Sections that moved in 1.1 land on their new home.
    if (selected === 'panel-settings-system') selected = 'panel-settings-storage';
    if (selected === 'panel-settings-channels') selected = 'panel-settings-general';
    if (selected === 'panel-settings-identity') selected = 'panel-settings-general';
    selectSettingsSection(selected, { skipStore: true });
    if (_settingsMobileModeActive()) _settingsMobileDetailActive = false;
    syncSettingsMobileLayout();
}

function syncSettingsMobileLayout() {
    var page = document.getElementById('settings-page');
    if (!page) return;
    var mobile = _settingsMobileModeActive();
    var open = mobile && _settingsMobileDetailActive;
    page.classList.toggle('settings-mobile-detail-active', open);
    var detail = page.querySelector('.settings-detail-pane');
    var pane = page.querySelector('.settings-nav-pane');
    if (detail) detail.classList.toggle('is-open', open);
    if (pane) pane.classList.toggle('is-covered', open);
}

function isSettingsMobileDetailActive() {
    return _settingsMobileModeActive() && _settingsMobileDetailActive;
}

function showSettingsMobileSectionIndex(opts) {
    opts = opts || {};
    _settingsMobileDetailActive = false;
    syncSettingsMobileLayout();
    if (_settingsMobileModeActive()) {
        var scroller = document.querySelector('#view-settings .settings-nav-pane .ui-pane-scroll');
        if (scroller) scroller.scrollTop = 0;
        if (opts.restoreFocus !== false) {
            var activeItem = document.querySelector('#view-settings .settings-index [data-settings-panel="' + _settingsShownPanelId + '"]');
            if (activeItem) requestAnimationFrame(function() { activeItem.focus({ preventScroll: true }); });
        }
    }
}

function loadSettingsInterfaces() {
    loadSettingsInterfacesWithRetry(1);
}

// Settings › Network › Relay for other devices reads the transport payload.
function loadSettingsInterfacesWithRetry(retries) {
    RS.invoke('api_hub_interfaces').then(function(ifaces) {
        if (ifaces && ifaces.transport) applyTransportModePayload(ifaces.transport);
    }).catch(function() {
        if (retries > 0) setTimeout(function() { loadSettingsInterfacesWithRetry(retries - 1); }, 2000);
    });
}

// Reusable so it can re-run after enabling (to surface a
// permission-needed / adapter-unavailable probe result instead of a stuck
// "Scanning…") and after a webview reload (to rehydrate the peer rows, which
// otherwise only exist from the per-peer event stream during the session).
window.refreshBlePeerStatus = function() {
    return RS.invoke('api_ble_peer_status').then(function(data) {
        window._blePeerAvailable = !!data.available;
        window._blePeerEnabled = !!data.enabled;
        window._blePeerPaused = !!data.paused;
        if (data.expires_at !== undefined) window._blePeerExpiresAt = data.expires_at;
        if (data.duration !== undefined) window._blePeerDuration = data.duration;
        if (data.state) window._blePeerState = data.state;
        if (typeof data.peer_count === 'number') window._blePeerCount = data.peer_count;
        // Rehydrate connected-peer rows from the snapshot.
        if (data.enabled && Array.isArray(data.peers)) {
            window._blePeers = window._blePeers || {};
            data.peers.forEach(function(p) {
                if (!p || !p.address) return;
                var prior = window._blePeers[p.address] || {};
                window._blePeers[p.address] = {
                    address: p.address,
                    identity_hash: p.identity_hash || prior.identity_hash || '',
                    provisional_identity_hash: '',
                    readiness: 'routable',
                    routable: true,
                    protocol: prior.protocol || 'Ratspeak',
                    rssi: prior.rssi,
                    connected: true,
                    connected_at: prior.connected_at || Date.now(),
                };
            });
            if (typeof _renderConnectionsFromCache === 'function') _renderConnectionsFromCache();
        }
        if (typeof updateBlePeerToggle === 'function') updateBlePeerToggle();
        if (typeof _refreshBlePeerSectionState === 'function') _refreshBlePeerSectionState();
    }).catch(function() {});
};
window.refreshBlePeerStatus();

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
        showToast('Contact card is not ready yet', 'toast-warning', 2500);
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
    var value = document.getElementById('settings-identity-value');
    if (!value) return;
    var activeName = active && typeof identityDisplayName === 'function'
        ? identityDisplayName(active)
        : (active && (active.display_name || active.nickname || ''));
    value.textContent = active ? (activeName || 'Unnamed') : '';
}
window.syncSettingsIdentityActions = syncSettingsIdentityActions;

function settingsCurrentStatusValue() {
    if (typeof resolveActiveProfileStatus === 'function') {
        return String(resolveActiveProfileStatus() || '').trim();
    }
    return '';
}

function syncSettingsIdentityStatus() {
    syncSettingsIdentityActions();
}

(function() {
    var row = document.getElementById('settings-open-identity');
    if (row) row.addEventListener('click', function() {
        if (typeof openIdentityPage === 'function') openIdentityPage();
        else switchView('identity');
    });
})();

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
    if (typeof RS !== 'undefined' && RS.text && typeof RS.text.utf8Length === 'function') {
        return RS.text.utf8Length(value);
    }
    if (window.TextEncoder) return new TextEncoder().encode(value).length;
    return new Blob([value]).size;
}

function trimProfileStatusToByteLimit(value, limit) {
    value = String(value || '');
    limit = limit || PROFILE_STATUS_MAX_BYTES;
    if (profileStatusByteLength(value) <= limit) return value;
    if (typeof RS !== 'undefined' && RS.text && typeof RS.text.truncateUtf8 === 'function') {
        return RS.text.truncateUtf8(value, limit);
    }

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

function renderActiveProfileStatus(status) {
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

function saveIdentityStatus(nextStatus) {
    var identityId = typeof activeIdentityHash !== 'undefined' ? activeIdentityHash : null;
    return RS.invoke('set_identity_status', { status: nextStatus, identityId: identityId });
}

function openIdentityStatusEditor() {
    if (typeof _rsBuildSheet !== 'function') return;

    var initialStatus = trimProfileStatusToByteLimit(
        String(resolveActiveProfileStatus() || '').trim(),
        PROFILE_STATUS_MAX_BYTES
    );
    var built = _rsBuildSheet({
        title: initialStatus ? 'Edit status' : 'Set status',
        showTitle: false
    }, function() {});

    built.overlay.addEventListener('click', function(e) {
        if (e.target === built.overlay) built.dismiss(null);
    });

    var label = document.createElement('label');
    label.className = 'rs-dialog-field-label';
    label.htmlFor = 'profile-status-input';
    label.textContent = 'Status';

    var textarea = document.createElement('textarea');
    textarea.id = 'profile-status-input';
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
    var saveBtn = null;
    var clearStatusBtn = null;
    var isSubmitting = false;

    function updateCounter() {
        var trimmed = trimProfileStatusToByteLimit(textarea.value, PROFILE_STATUS_MAX_BYTES);
        if (trimmed !== textarea.value) textarea.value = trimmed;
        var bytes = profileStatusByteLength(textarea.value);
        counter.textContent = bytes + '/' + PROFILE_STATUS_MAX_BYTES;
        counter.classList.toggle('at-limit', bytes >= PROFILE_STATUS_MAX_BYTES);
        if (saveBtn && !isSubmitting) {
            saveBtn.disabled = textarea.value.trim() === initialStatus;
        }
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

    var saveLabel = initialStatus ? 'Save changes' : 'Set status';
    saveBtn = document.createElement('button');
    saveBtn.className = 'rs-dialog-confirm';
    saveBtn.textContent = saveLabel;
    saveBtn.addEventListener('click', function() {
        submitStatus(textarea.value);
    });

    function setSubmitting(submitting, isClearing) {
        isSubmitting = submitting;
        cancelBtn.disabled = submitting;
        saveBtn.disabled = submitting;
        saveBtn.textContent = submitting
            ? (isClearing ? 'Clearing...' : 'Saving...')
            : saveLabel;
        if (clearStatusBtn) clearStatusBtn.disabled = submitting;
        if (!submitting) updateCounter();
    }

    function submitStatus(value) {
        var nextStatus = trimProfileStatusToByteLimit(String(value || '').trim(), PROFILE_STATUS_MAX_BYTES);
        var isClearing = !!initialStatus && !nextStatus;
        textarea.value = nextStatus;
        updateCounter();
        setSubmitting(true, isClearing);
        saveIdentityStatus(nextStatus).then(function(result) {
            var savedStatus = profileStatusFromPayload(result);
            setActiveProfileStatus(savedStatus === null ? nextStatus : savedStatus);
            built.dismiss(nextStatus);
            if (typeof loadIdentities === 'function') loadIdentities();
        }).catch(function(err) {
            setSubmitting(false, false);
            if (typeof showToast === 'function') {
                showToast(
                    (err && err.message) ? err.message : (isClearing ? 'Could not clear status' : 'Could not save status'),
                    'toast-error',
                    3000
                );
            }
        });
    }

    if (initialStatus) {
        clearStatusBtn = document.createElement('button');
        clearStatusBtn.className = 'rs-dialog-cancel rs-dialog-clear-status';
        clearStatusBtn.textContent = 'Clear status';
        clearStatusBtn.addEventListener('click', function() { submitStatus(''); });
        built.footer.appendChild(clearStatusBtn);
    }
    built.footer.classList.add('profile-status-editor-footer');
    built.footer.appendChild(cancelBtn);
    built.footer.appendChild(saveBtn);
    updateCounter();

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
    setActiveProfileStatus(resolveActiveProfileStatus(status));
    if (hash && window.RS && RS.shell) {
        RS.shell.setIdentity(hash, displayName || localStorage.getItem('ratspeak_identity_name') || 'Unnamed');
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
        RS.ui.applyTransportModePayload('transport-mode-seg', data, { toastSuppressed: true });
    }
}

(function() {
    var seg = document.getElementById('transport-mode-seg');
    if (!seg) return;
    _settingsBindSeg(seg, function(mode) {
        if (RS.ui && typeof RS.ui.setTransportMode === 'function') RS.ui.setTransportMode(seg, mode);
    });
})();

// Native NetworkCallback / NWPathMonitor publishes directly to Rust; the
// WebView only observes transport-policy updates.

RS.listen('transport_mode_updated', function(data) {
    applyTransportModePayload(data);
});

var _announceLabels = {
    0: 'Never',
    900: '15 minutes',
    1800: '30 minutes',
    3600: '1 hour'
};

function _announceLabel(secs) {
    secs = parseInt(secs, 10) || 0;
    if (_announceLabels[secs] !== undefined) return _announceLabels[secs];
    if (secs <= 0) return 'Never';
    var hours = secs / 3600;
    if (hours >= 1 && hours === Math.floor(hours)) return hours + ' hours';
    return Math.round(secs / 60) + ' minutes';
}

// An inline select's value: the visible words and the stored value.
function _settingsSetSelectValue(select, text, value) {
    if (!select) return;
    var v = select.querySelector('.v');
    if (v) v.textContent = text;
    else select.textContent = text;
    if (value !== undefined) select.setAttribute('data-value', value);
    var label = select.getAttribute('data-label') || (select.getAttribute('aria-label') || '').split(':')[0];
    select.setAttribute('data-label', label);
    select.setAttribute('aria-label', label + ': ' + text);
}

// One menu for every inline select: a trailing check on the current value.
function _settingsOpenSelect(select, title, choices, current, onPick) {
    if (!select || !RS.kit || typeof RS.kit.menu !== 'function') return;
    RS.kit.menu(select, {
        title: title,
        items: choices.map(function(choice) {
            if (choice.separator) return { separator: true };
            return {
                label: choice.label,
                hint: choice.hint,
                checked: String(choice.value) === String(current),
                onSelect: function() { onPick(choice.value); }
            };
        })
    });
}

var _settingsAnnounceSelect = document.getElementById('auto-announce-select');
if (_settingsAnnounceSelect) {
    _settingsAnnounceSelect.addEventListener('click', function() {
        var current = parseInt(_settingsAnnounceSelect.getAttribute('data-value'), 10);
        if (isNaN(current)) current = 3600;
        var preset = [0, 900, 1800, 3600].indexOf(current) !== -1;
        _settingsOpenSelect(_settingsAnnounceSelect, 'Auto-announce', [
            { label: 'Never', value: '0', hint: 'Announce only when you choose.' },
            { label: '15 minutes', value: '900' },
            { label: '30 minutes', value: '1800' },
            { label: '1 hour', value: '3600' },
            { separator: true },
            { label: preset ? 'Custom…' : 'Custom: ' + _announceLabel(current), value: 'custom', hint: 'Between 1 and 48 hours.' }
        ], preset ? String(current) : 'custom', function(val) {
            var pending = val === 'custom'
                ? rsPrompt({
                    title: 'Custom interval',
                    message: 'Hours between announces, from 1 to 48.',
                    placeholder: 'e.g. 2',
                    confirmText: 'Set'
                }).then(function(input) {
                    if (input === null || input.trim() === '') return null;
                    var hours = parseInt(input, 10);
                    if (isNaN(hours) || hours < 1) hours = 1;
                    if (hours > 48) hours = 48;
                    return String(hours * 3600);
                })
                : Promise.resolve(val);
            pending.then(function(secs) {
                if (secs === null || secs === undefined) return;
                var interval = parseInt(secs, 10);
                _settingsSetSelectValue(_settingsAnnounceSelect, _announceLabel(interval), interval);
                RS.invoke('set_auto_announce', { interval: interval }).catch(function(err) {
                    showToast((err && err.message) || 'Could not update the announce interval', 'toast-error', 8000);
                });
            });
        });
    });
}

RS.listen('auto_announce_updated', function(data) {
    applyAppSettingsPayload({ auto_announce_interval: data && data.interval });
});

function applyAppSettingsPayload(data) {
    if (!data) return;
    var announce = document.getElementById('auto-announce-select');
    var interval = data.auto_announce_interval !== undefined ? data.auto_announce_interval : data.interval;
    if (announce && interval !== undefined) {
        var secs = parseInt(interval, 10);
        _settingsSetSelectValue(announce, _announceLabel(secs), secs);
    }
    var usageToggle = document.getElementById('announce-ratspeak-usage-toggle');
    if (usageToggle && data.announce_ratspeak_usage !== undefined) {
        usageToggle.checked = !!data.announce_ratspeak_usage;
    }
    var androidRnodeAutoResumeToggle = document.getElementById('android-rnode-auto-resume-toggle');
    if (androidRnodeAutoResumeToggle && data.android_ble_rnode_auto_resume !== undefined) {
        androidRnodeAutoResumeToggle.checked = !!data.android_ble_rnode_auto_resume;
    }
    if (data.activity_identity_protection !== undefined) {
        adoptActivityIdentityProtectionFromBackend(data.activity_identity_protection);
    }
    if (data.hide_known_spam_peers !== undefined) {
        adoptHideKnownSpamPeersFromBackend(data.hide_known_spam_peers);
    }
    if (data.paper_messages_enabled !== undefined) {
        adoptPaperMessagesFromBackend(data.paper_messages_enabled);
    }
    var lxmfLimitToggle = document.getElementById('lxmf-limit-1mb-toggle');
    if (lxmfLimitToggle && data.lxmf_limit_1mb !== undefined) {
        lxmfLimitToggle.checked = !!data.lxmf_limit_1mb;
    }
    var keepTryingToggle = document.getElementById('lxmf-keep-trying-toggle');
    if (keepTryingToggle && data.lxmf_keep_trying !== undefined) {
        keepTryingToggle.checked = !!data.lxmf_keep_trying;
    }
    if (data.text_scale_percent !== undefined && RS.textScale) {
        RS.textScale.commit(data.text_scale_percent);
    }
    if (RS.appearance && (data.theme_family !== undefined || data.theme_mode !== undefined)) {
        var appearance = RS.appearance.get();
        RS.appearance.commit(
            data.theme_family !== undefined ? data.theme_family : appearance.family,
            data.theme_mode !== undefined ? data.theme_mode : appearance.preference
        );
        syncAppearanceControls();
    }
    if (data.developer_mode !== undefined) {
        adoptDeveloperModeFromBackend(data.developer_mode);
    }
    if (data.window_decorations !== undefined) {
        adoptWindowDecorationsFromBackend(data.window_decorations);
    }
    if (data.close_button_action !== undefined) {
        adoptCloseButtonActionFromBackend(data.close_button_action);
    }
    if (data.channel_hosting_enabled !== undefined) {
        adoptChannelHostingFromBackend(data.channel_hosting_enabled);
    }
    if (data.update_last_checked !== undefined) {
        adoptUpdateLastCheckedFromBackend(data.update_last_checked);
    }
    if (typeof RS.adoptUiState === 'function') RS.adoptUiState(data);
}


(function() {
    var usageToggle = document.getElementById('announce-ratspeak-usage-toggle');
    var androidRnodeAutoResumeRow = document.getElementById('settings-row-android-rnode-auto-resume');
    var androidRnodeAutoResumeToggle = document.getElementById('android-rnode-auto-resume-toggle');
    var lxmfLimitToggle = document.getElementById('lxmf-limit-1mb-toggle');
    var keepTryingToggle = document.getElementById('lxmf-keep-trying-toggle');
    if (androidRnodeAutoResumeRow) androidRnodeAutoResumeRow.hidden = !isAndroid();
    initActivityIdentityProtectionToggle();
    initHideKnownSpamPeersToggle();
    initPaperMessagesToggle();
    RS.invoke('api_app_settings').then(applyAppSettingsPayload).catch(function() {});
    if (lxmfLimitToggle) {
        lxmfLimitToggle.addEventListener('change', function() {
            var enabled = !!lxmfLimitToggle.checked;
            RS.invoke('set_lxmf_limit_1mb', { enabled: enabled })
                .then(applyAppSettingsPayload)
                .catch(function(error) {
                    lxmfLimitToggle.checked = !enabled;
                    showToast((error && error.message) || 'Could not update the message limit', 'toast-error', 8000);
                });
        });
    }
    if (keepTryingToggle) {
        keepTryingToggle.addEventListener('change', function() {
            var enabled = !!keepTryingToggle.checked;
            RS.invoke('set_lxmf_keep_trying', { enabled: enabled })
                .then(applyAppSettingsPayload)
                .catch(function(error) {
                    keepTryingToggle.checked = !enabled;
                    showToast((error && error.message) || 'Could not update Keep trying to send', 'toast-error', 8000);
                });
        });
    }
    if (androidRnodeAutoResumeToggle) {
        androidRnodeAutoResumeToggle.addEventListener('change', function() {
            var enabled = !!androidRnodeAutoResumeToggle.checked;
            RS.invoke('set_android_ble_rnode_auto_resume', { enabled: enabled })
                .then(applyAppSettingsPayload)
                .catch(function(error) {
                    androidRnodeAutoResumeToggle.checked = !enabled;
                    showToast((error && error.message) || 'Could not update RNode reconnect behavior', 'toast-error', 8000);
                });
        });
    }
    if (!usageToggle) return;
    usageToggle.addEventListener('change', function() {
        var enabled = !!usageToggle.checked;
        RS.invoke('set_announce_ratspeak_usage', { enabled: enabled })
            .then(function(data) {
                if (data && data.enabled !== undefined) usageToggle.checked = !!data.enabled;
            })
            .catch(function(err) {
                usageToggle.checked = !enabled;
                showToast((err && err.message) || 'Could not update the privacy setting', 'toast-error', 8000);
            });
    });
})();

RS.listen('app_settings_updated', applyAppSettingsPayload);

function _settingsNotificationActionForState(state) {
    if (state === 'granted') return { hidden: false, disabled: false, label: 'Review' };
    if (state === 'prompt') return { hidden: false, disabled: false, label: 'Allow' };
    if (state === 'denied') return { hidden: false, disabled: false, label: 'Open Settings' };
    return { hidden: false, disabled: true, label: 'Unavailable' };
}

function _settingsNotificationPresentation(enabled, state) {
    if (!enabled) return { hidden: true, disabled: true, label: 'Allow' };
    return _settingsNotificationActionForState(state);
}

var NOTIFICATION_PREVIEW_LABELS = {
    name_and_message: 'Name and message',
    name_only: 'Name only',
    neither: 'Neither'
};

// What message alerts show. Returns the row's controls.
function _initNotificationPreview(row, badge) {
    function show(value) {
        var known = Object.prototype.hasOwnProperty.call(NOTIFICATION_PREVIEW_LABELS, value);
        var preview = known ? value : 'name_and_message';
        _settingsSetSelectValue(badge, NOTIFICATION_PREVIEW_LABELS[preview], preview);
    }
    badge.addEventListener('click', function() {
        var previous = badge.getAttribute('data-value') || 'name_and_message';
        _settingsOpenSelect(badge, 'Previews', [
            { label: 'Name and message', value: 'name_and_message' },
            { label: 'Name only', value: 'name_only', hint: '“Message from Alice”, without the text.' },
            { label: 'Neither', value: 'neither', hint: 'Just “New message”. A call always shows who is calling.' }
        ], previous, function(value) {
            if (!value || value === previous) return;
            show(value);
            return RS.invoke('set_notification_preview', { preview: value }).then(function(settings) {
                show(settings && settings.preview);
            }, function(err) {
                show(previous);
                showToast(toastErrorCopy(err, 'Could not change previews'), 'toast-error', 4000);
            });
        });
    });
    return {
        show: show,
        visible: function(enabled) { row.hidden = !enabled; }
    };
}

function _settingsBatteryPresentation(state) {
    if (state === 'exempt') return {
        label: 'Review',
        description: 'Battery optimization is off. Android or the device maker can still stop Ratspeak.'
    };
    if (state === 'optimized') return {
        label: 'Review',
        description: 'Battery optimization can delay background messages. Review Android settings.'
    };
    return {
        label: 'Review',
        description: 'Battery access could not be checked. Review Android settings.'
    };
}

(function() {
    var _notifRow = document.getElementById('settings-row-notifications');
    var _notifToggle = document.getElementById('desktop-notifications-toggle');
    var _notifAction = document.getElementById('settings-notification-action');
    var _keepRow = document.getElementById('settings-row-keep-connected');
    var _keepAction = document.getElementById('settings-keep-connected-action');
    var _keepDesc = document.getElementById('settings-keep-connected-desc');
    if (!_notifRow || !_notifToggle) return;
    var _isMobile = typeof isTauriMobile === 'function'
        ? isTauriMobile()
        : !!window.__RATSPEAK_MOBILE__;
    var _android = typeof hasAndroidBridge === 'function' && hasAndroidBridge();
    var _notifAuthorization = 'unavailable';
    var _notifPreferenceEnabled = true;
    _notifRow.hidden = false;
    var _previewRow = document.getElementById('settings-row-notification-preview');
    var _previewBadge = document.getElementById('notification-preview-select');
    var _preview = _previewRow && _previewBadge
        ? _initNotificationPreview(_previewRow, _previewBadge)
        : { show: function() {}, visible: function() {} };

    function _refreshNotificationAuthorization() {
        if (!_isMobile || typeof rsNotify === 'undefined') return;
        if (!_notifPreferenceEnabled) {
            if (_notifAction) _notifAction.hidden = true;
            return;
        }
        rsNotify.permissionState().then(function(state) {
            _notifAuthorization = state;
            if (_notifAction) {
                var action = _settingsNotificationPresentation(_notifPreferenceEnabled, state);
                _notifAction.hidden = !!action.hidden;
                _notifAction.disabled = action.disabled;
                _notifAction.textContent = action.label;
            }
        });
    }

    function _refreshKeepConnected() {
        if (!_android || !_keepRow || !_keepAction || !_keepDesc) return;
        _keepRow.hidden = false;
        var status = 'unavailable';
        try { status = window.RatspeakAndroid.batteryOptimizationStatus(); } catch (_) {}
        var presentation = _settingsBatteryPresentation(status);
        _keepAction.textContent = presentation.label;
        _keepDesc.textContent = presentation.description;
    }

    RS.invoke('api_notification_settings').then(function(data) {
        if (!data || data.enabled === undefined) return;
        _notifPreferenceEnabled = !!data.enabled;
        _notifToggle.checked = _notifPreferenceEnabled;
        _preview.show(data.preview);
        _preview.visible(_notifPreferenceEnabled);
        if (typeof rsNotify !== 'undefined') rsNotify.setEnabled(_notifPreferenceEnabled);
        if (_notifPreferenceEnabled && typeof rsNotify !== 'undefined' && rsNotify.available()) {
            _refreshNotificationAuthorization();
        } else if (_notifAction) {
            _notifAction.hidden = true;
        }
    }).catch(function() {});

    _notifToggle.addEventListener('change', function() {
        var enabled = !!_notifToggle.checked;
        _notifPreferenceEnabled = enabled;
        _preview.visible(enabled);
        if (typeof rsNotify !== 'undefined') rsNotify.setEnabled(enabled);
        RS.invoke('set_desktop_notifications', { enabled: enabled }).catch(function() {});
        if (enabled && typeof rsNotify !== 'undefined' && rsNotify.available()) {
            rsNotify.requestPermission().then(function() {
                setTimeout(_refreshNotificationAuthorization, 350);
            });
        } else if (_notifAction) {
            _notifAction.hidden = true;
        }
    });

    if (_notifAction) _notifAction.addEventListener('click', function() {
        if (_notifAuthorization === 'prompt' && typeof rsNotify !== 'undefined') {
            rsNotify.requestPermission().then(function() {
                setTimeout(_refreshNotificationAuthorization, 350);
            });
            return;
        }
        if (_android && typeof window.RatspeakAndroid.openNotificationSettings === 'function') {
            try { window.RatspeakAndroid.openNotificationSettings(); } catch (_) {}
        } else {
            RS.invoke('open_mobile_app_settings').catch(function() {
                showToast('Could not open notification settings', 'toast-error', 4000);
            });
        }
    });

    if (_keepAction) _keepAction.addEventListener('click', function() {
        if (!_android || typeof window.RatspeakAndroid.requestBatteryOptimizationExemption !== 'function') return;
        try {
            if (!window.RatspeakAndroid.requestBatteryOptimizationExemption()) {
                showToast('Could not open battery settings', 'toast-error', 4000);
            }
        } catch (_) {
            showToast('Could not open battery settings', 'toast-error', 4000);
        }
    });

    document.addEventListener('visibilitychange', function() {
        if (!document.hidden) {
            _refreshNotificationAuthorization();
            _refreshKeepConnected();
        }
    });
    document.addEventListener('rs-notification-permission-changed', _refreshNotificationAuthorization);
    window.addEventListener('focus', function() {
        _refreshNotificationAuthorization();
        _refreshKeepConnected();
    });
    _refreshNotificationAuthorization();
    _refreshKeepConnected();
})();

RS.listen('desktop_notifications_updated', function(data) {
    var toggle = document.getElementById('desktop-notifications-toggle');
    if (!toggle || !data || data.enabled === undefined) return;
    toggle.checked = !!data.enabled;
    if (typeof rsNotify !== 'undefined') rsNotify.setEnabled(!!data.enabled);
});

syncSettingsIdentityActions();

function _showBulkDeleteFailure(err, failMsg) {
    if (err && err.code === 'files_not_removed') {
        showToast('Messages deleted. Some files could not be removed yet; Ratspeak will retry.',
            'toast-warning', 5000);
        return true;
    }
    showToast(failMsg, 'toast-error', 3000);
    return false;
}

function clearAtOnce(commandName, successMsg, failMsg) {
    RS.invoke(commandName).then(function() {
        showToast(successMsg, 'toast-success', 3000);
    }).catch(function(err) {
        _showBulkDeleteFailure(err, failMsg);
    });
}

function deleteWithConfirm(commandName, title, message, confirmText, successMsg, failMsg) {
    rsConfirm({ title: title, message: message, danger: true, confirmText: confirmText }).then(function(ok) {
        if (!ok) return;
        RS.invoke(commandName).then(function() {
            if (typeof haptic === 'function') haptic('success');
            showToast(successMsg, 'toast-success', 3000);
        }).catch(function(err) {
            var partial = _showBulkDeleteFailure(err, failMsg);
            if (typeof haptic === 'function') haptic(partial ? 'warning' : 'error');
        });
    });
}

var clearPathsBtn = document.getElementById('settings-clear-paths');
if (clearPathsBtn) {
    clearPathsBtn.addEventListener('click', function() {
        clearAtOnce('api_clear_paths', 'Routes cleared', 'Could not clear routes');
    });
}

var clearAnnouncesBtn = document.getElementById('settings-clear-announces');
if (clearAnnouncesBtn) {
    clearAnnouncesBtn.addEventListener('click', function() {
        clearAtOnce('api_clear_announces', 'Peers cleared', 'Could not clear peers');
    });
}

var clearMessagesBtn = document.getElementById('settings-clear-messages');
if (clearMessagesBtn) {
    clearMessagesBtn.addEventListener('click', function() {
        deleteWithConfirm('api_clear_messages', 'Delete all messages?',
            'Every conversation on this device is deleted. This can’t be undone.',
            'Delete messages', 'All messages deleted', 'Could not delete messages');
    });
}

var clearContactsBtn = document.getElementById('settings-clear-contacts');
if (clearContactsBtn) {
    clearContactsBtn.addEventListener('click', function() {
        deleteWithConfirm('api_clear_contacts', 'Delete all contacts?',
            'Your saved contacts are deleted. Blocked people stay blocked. This can’t be undone.',
            'Delete contacts', 'All contacts deleted', 'Could not delete contacts');
    });
}

var resetDatabaseBtn = document.getElementById('settings-reset-database');
if (resetDatabaseBtn) {
    resetDatabaseBtn.addEventListener('click', function() {
        deleteWithConfirm('api_reset_database', 'Delete messages and contacts?',
            'Every conversation and saved contact is deleted. Blocked people stay blocked. This can’t be undone.',
            'Delete both', 'All messages and contacts deleted', 'Could not delete messages and contacts');
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
var _announcePending = false;

function setAnnounceLabel(btn, text) {
    if (!btn) return;
    var labelEl = btn.querySelector('span:not([aria-hidden])') || btn.querySelector('span');
    if (labelEl) labelEl.textContent = text;
    else btn.textContent = text;
}

// Returns true if IPC fired, false if rate-limited or no online interface.
function tryTriggerAnnounce() {
    if (_announcePending) {
        showToast('Announce already queued', 'toast-info', 2500);
        return false;
    }
    if (Date.now() - _lastAnnounceTime < ANNOUNCE_COOLDOWN) {
        showRateLimitedToast();
        return false;
    }
    if (_anyInterfaceOnline === false) {
        showToast('Connect to a network first', 'toast-warning', 3000);
        return false;
    }
    // Set the shared guard before IPC. Every manual entry point calls this
    // function, so two UI surfaces cannot cross the native boundary together.
    _announcePending = true;
    RS.invoke('trigger_announce').then(function(data) {
        handleManualAnnounceResult(data || { success: false, error: 'invalid_response' });
    }).catch(function(err) {
        handleManualAnnounceResult({
            success: false,
            error: 'ipc_error',
            message: (err && err.message) || 'Could not send the announce'
        });
    });
    return true;
}

// The originating IPC response is the single completion owner. A WebView can
// miss a broadcast while suspending; tying this guard to a second event left
// the visible button reset while `_announcePending` remained stuck forever.
function handleManualAnnounceResult(data) {
    data = data || { success: false, error: 'invalid_response' };
    _announcePending = false;
    var networkBtn = document.getElementById('network-announce-btn');
    if (networkBtn && networkBtn.dataset) delete networkBtn.dataset.announcePending;
    // Pop the long-press origin (nav.js _holdLoop); ignore if stale (>5s).
    var origin = (typeof _pendingAnnounceOrigin !== 'undefined') ? _pendingAnnounceOrigin : null;
    if (origin && Date.now() - origin.t > 5000) origin = null;
    if (typeof _pendingAnnounceOrigin !== 'undefined') _pendingAnnounceOrigin = null;

    if (data.success) {
        _lastAnnounceTime = Date.now();
        window.dispatchEvent(new CustomEvent('ratspeak-manual-announce-result', { detail: data }));
        var alreadyQueued = data.disposition === 'already_queued' || data.disposition === 'deferred';
        if (typeof haptic === 'function') haptic(alreadyQueued ? 'light' : 'success');
        showToast(
            alreadyQueued ? 'Announce already queued' : 'Announce queued',
            alreadyQueued ? 'toast-info' : 'toast-success',
            4000
        );
        // The request is admitted by the presence coordinator. Component
        // queueing and interface-specific transmission continue asynchronously.
        if (!alreadyQueued && origin && typeof showAnnounceAnimation === 'function') {
            showAnnounceAnimation(origin.el, origin.cx, origin.cy);
        }
        if (networkBtn) {
            setAnnounceLabel(networkBtn, alreadyQueued ? 'Already queued' : 'Queued');
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
        showToast('Connect to a network first', 'toast-warning', 3000);
        // Frontend cache disagreed with backend; play dampened animation for closure.
        if (origin && typeof showAnnounceFailAnimation === 'function') {
            showAnnounceFailAnimation(origin.el, origin.cx, origin.cy);
        }
        if (networkBtn) {
            setAnnounceLabel(networkBtn, 'Announce');
            networkBtn.disabled = false;
        }
    } else if (data.error === 'not_ready') {
        if (typeof haptic === 'function') haptic('error');
        showToast('Ratspeak is still starting. Try Announce again.', 'toast-warning', 4000);
        if (origin && typeof showAnnounceFailAnimation === 'function') {
            showAnnounceFailAnimation(origin.el, origin.cx, origin.cy);
        }
        if (networkBtn) {
            setAnnounceLabel(networkBtn, 'Announce');
            networkBtn.disabled = false;
        }
    } else {
        if (typeof haptic === 'function') haptic('error');
        var failureMessage = data.error === 'busy'
            ? 'Ratspeak is busy. Try Announce again in a moment.'
            : (data.message || 'Could not send the announce');
        showToast(failureMessage, data.error === 'busy' ? 'toast-warning' : 'toast-error', 5000);
        if (origin && typeof showAnnounceFailAnimation === 'function') {
            showAnnounceFailAnimation(origin.el, origin.cx, origin.cy);
        }
        if (networkBtn) {
            setAnnounceLabel(networkBtn, 'Announce');
            networkBtn.disabled = false;
        }
    }
}

function confirmDangerAction(action, onClose) {
    function _close() { if (typeof onClose === 'function') try { onClose(); } catch (_) {} }
    if (action !== 'factory-reset') return;
    if (typeof closeDangerZone === 'function') closeDangerZone();
    rsConfirm({
        title: 'Reset Ratspeak?',
        message: 'This deletes every identity on this device, with all messages, contacts, settings, and history, and starts Ratspeak fresh. Identities on a hardware key aren’t erased. This can’t be undone.',
        danger: true,
        confirmText: 'Reset Ratspeak'
    }).then(function(ok) {
        if (!ok) { _close(); return; }
        return rsConfirm({
            title: 'Delete everything?',
            message: 'Every identity and all data on this device are deleted for good.',
            danger: true,
            confirmText: 'Delete everything'
        });
    }).then(function(ok) {
        if (ok === undefined) return;
        if (!ok) { _close(); return; }
        if (typeof haptic === 'function') haptic('warning');
        showToast('Resetting Ratspeak…', 'toast-progress', 5000);
        RS.invoke('api_factory_reset')
            .then(function() {
                if (typeof clearFirstRunAnnounceHintDone === 'function') clearFirstRunAnnounceHintDone();
                if (RS.appearance) {
                    RS.appearance.commit(
                        RS.appearance.DEFAULT_FAMILY,
                        RS.appearance.DEFAULT_MODE
                    );
                }
                // reload() re-requests tauri://localhost/. location.href='/'
                // breaks on dev-contaminated builds (TAURI_CONFIG leak → dev URL).
                setTimeout(function() { window.location.reload(); }, 1500);
            })
            .catch(function() {
                if (typeof haptic === 'function') haptic('error');
                showToast('Could not reset Ratspeak', 'toast-error', 5000);
                _close();
            });
    });
}

var _appearanceControlsInitialized = false;
var _appearanceSaving = false;
var _hapticsToggleInitialized = false;
var _textScaleInitialized = false;
var _textScaleSaving = false;

function renderThemeFamilyPicker() {
    var grid = document.getElementById('theme-family-grid');
    if (!grid || grid.childElementCount || !RS.appearance) return;

    RS.appearance.families.forEach(function(family) {
        var tile = document.createElement('button');
        tile.type = 'button';
        tile.className = 'settings-choice';
        tile.setAttribute('role', 'radio');
        tile.setAttribute('aria-checked', 'false');
        tile.setAttribute('data-family', family.id);

        var swatch = document.createElement('span');
        swatch.className = 'settings-swatch';
        swatch.setAttribute('aria-hidden', 'true');
        [family.preview.light, family.preview.dark].forEach(function(colors) {
            var half = document.createElement('span');
            half.style.setProperty('--swatch-bg', colors[0]);
            half.style.setProperty('--swatch-panel', colors[1]);
            half.style.setProperty('--swatch-accent', colors[2]);
            swatch.appendChild(half);
        });

        var name = document.createElement('span');
        name.className = 'settings-choice-name';
        name.textContent = family.name;

        tile.appendChild(swatch);
        tile.appendChild(name);
        grid.appendChild(tile);
    });
}

// Choice tiles: the selection fill and a check, nothing else.
function _settingsSyncChoice(tile, selected, busy) {
    tile.setAttribute('aria-checked', selected ? 'true' : 'false');
    tile.disabled = !!busy;
    var name = tile.querySelector('.settings-choice-name');
    if (!name) return;
    var check = name.querySelector('.ui-ico');
    if (selected && !check && RS.kit) name.insertAdjacentHTML('beforeend', RS.kit.icon('check'));
    if (!selected && check) check.remove();
}

function syncAppearanceControls() {
    var toggle = document.getElementById('theme-toggle');
    var grid = document.getElementById('theme-family-grid');
    if (!toggle || !grid || !RS.appearance) return;

    renderThemeFamilyPicker();
    var current = RS.appearance.get();
    grid.querySelectorAll('.settings-choice[data-family]').forEach(function(tile) {
        _settingsSyncChoice(tile, tile.getAttribute('data-family') === current.family, _appearanceSaving);
    });
    _settingsSyncSeg(toggle, current.preference, _appearanceSaving);
    grid.setAttribute('aria-busy', _appearanceSaving ? 'true' : 'false');
    toggle.setAttribute('aria-busy', _appearanceSaving ? 'true' : 'false');
}

function saveAppearance(family, preference) {
    if (_appearanceSaving || !RS.appearance) return;
    var previous = RS.appearance.get();
    var next = RS.appearance.commit(family, preference);
    _appearanceSaving = true;
    syncAppearanceControls();

    RS.invoke('set_appearance', {
        family: next.family,
        mode: next.preference
    }).then(function(result) {
        RS.appearance.commit(
            result && result.theme_family !== undefined ? result.theme_family : next.family,
            result && result.theme_mode !== undefined ? result.theme_mode : next.preference
        );
    }).catch(function(error) {
        RS.appearance.commit(previous.family, previous.preference);
        if (typeof showToast === 'function') {
            showToast((error && error.message) || 'Could not save appearance', 'toast-error', 4000);
        }
    }).then(function() {
        _appearanceSaving = false;
        syncAppearanceControls();
    });
}

function initThemeToggle() {
    var toggle = document.getElementById('theme-toggle');
    var grid = document.getElementById('theme-family-grid');
    if (!toggle || !grid || !RS.appearance) return;

    renderThemeFamilyPicker();
    syncAppearanceControls();

    if (_appearanceControlsInitialized) return;
    _appearanceControlsInitialized = true;

    grid.addEventListener('click', function(event) {
        var tile = event.target.closest('.settings-choice[data-family]');
        if (!tile || tile.disabled || tile.getAttribute('aria-checked') === 'true') return;
        saveAppearance(tile.getAttribute('data-family'), RS.appearance.get().preference);
    });
    _settingsBindSeg(toggle, function(mode) {
        saveAppearance(RS.appearance.get().family, mode);
    });
    window.addEventListener('ratspeak-theme-changed', syncAppearanceControls);
}

function initHapticsToggle() {
    var toggle = document.getElementById('haptics-enabled-toggle');
    if (!toggle) return;
    // Vibration means something only on a phone or tablet.
    var block = document.getElementById('settings-device-block');
    if (block) block.hidden = !((typeof isTauriMobile === 'function' && isTauriMobile()) || !!window.__RATSPEAK_MOBILE__);

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

function initTextScaleControl() {
    var group = document.getElementById('settings-text-scale-tiles');
    if (!group || !RS.textScale) return;
    var tiles = group.querySelectorAll('.settings-choice[data-scale]');

    function sync(value) {
        var percent = RS.textScale.normalize(value);
        tiles.forEach(function(tile) {
            tile.setAttribute('aria-checked', Number(tile.getAttribute('data-scale')) === percent ? 'true' : 'false');
            tile.disabled = _textScaleSaving;
        });
        group.setAttribute('aria-busy', _textScaleSaving ? 'true' : 'false');
    }

    function save(value) {
        if (_textScaleSaving) return;
        var previous = RS.textScale.get();
        var percent = RS.textScale.commit(value);
        _textScaleSaving = true;
        sync(percent);
        RS.invoke('set_text_scale', { percent: percent }).then(function(result) {
            RS.textScale.commit(result && result.percent !== undefined ? result.percent : percent);
        }).catch(function(error) {
            RS.textScale.commit(previous);
            if (typeof showToast === 'function') {
                showToast((error && error.message) || 'Could not save text size', 'toast-error', 4000);
            }
        }).then(function() {
            _textScaleSaving = false;
            sync(RS.textScale.get());
        });
    }

    sync(RS.textScale.get());
    if (_textScaleInitialized) return;
    _textScaleInitialized = true;

    group.addEventListener('click', function(event) {
        var tile = event.target.closest('.settings-choice[data-scale]');
        if (!tile || tile.disabled || tile.getAttribute('aria-checked') === 'true') return;
        save(Number(tile.getAttribute('data-scale')));
    });
    window.addEventListener('ratspeak-text-scale-changed', function(event) {
        if (event.detail) sync(event.detail.percent);
    });
}

document.addEventListener('DOMContentLoaded', function() {
    initThemeToggle();
    initHapticsToggle();
    initTextScaleControl();
    initSettingsSectionNav();
    renderSettingsVersion();
});

function updateBlockedCount() {
    RS.invoke('api_blocked_contacts').then(function(list) {
        var value = document.getElementById('settings-blocked-value');
        var n = Array.isArray(list) ? list.length : 0;
        if (value) value.textContent = n ? String(n) : 'None';
    }).catch(function() {});
}

function openBlockListModal() {
    if (!RS.kit || typeof RS.kit.dialog !== 'function') return;
    var allBlocked = [];
    var unlistenPromise = null;
    var built = RS.kit.dialog({
        title: 'Blocked people',
        size: 'md',
        className: 'block-list-sheet',
        focus: '#block-list-search',
        body: '<div class="ui-search" role="search">' + RS.kit.icon('search') +
                '<input type="search" id="block-list-search" placeholder="Search blocked people" aria-label="Search blocked people" autocomplete="off" autocapitalize="none" spellcheck="false"></div>' +
            '<div id="block-list-container" aria-live="polite"><div class="ui-empty ui-empty--inline" role="status"><span class="ui-spin" aria-hidden="true"></span><span>Loading</span></div></div>' +
            '<p class="ui-help">Blocked people can’t message you, call you, or see your status.</p>',
        onClose: function() {
            if (unlistenPromise) unlistenPromise.then(function(unlisten) { if (typeof unlisten === 'function') unlisten(); });
        }
    });
    var search = built.dialog.querySelector('#block-list-search');
    var container = built.dialog.querySelector('#block-list-container');

    function query() { return search ? search.value.toLowerCase().trim() : ''; }
    function refreshFromServer() {
        RS.invoke('api_blocked_contacts').then(function(list) {
            allBlocked = Array.isArray(list) ? list : [];
            renderBlockList(allBlocked, query());
            updateBlockedCount();
        }).catch(function() {
            container.innerHTML = '<p class="ui-help">Couldn’t load blocked people.</p>';
        });
    }
    unlistenPromise = RS.listen('blackhole_update', refreshFromServer);
    if (search) search.addEventListener('input', function() { renderBlockList(allBlocked, query()); });
    refreshFromServer();

    function nameOf(b) {
        return b.display_name || (RS.kit ? RS.kit.hash.short(b.hash) : b.hash);
    }

    function renderBlockList(list, q) {
        var filtered = q ? list.filter(function(b) {
            return (b.display_name || '').toLowerCase().indexOf(q) !== -1 ||
                (b.hash || '').toLowerCase().indexOf(q) !== -1;
        }) : list;
        if (!filtered.length) {
            container.innerHTML = '<div class="ui-empty ui-empty--inline"><span>' +
                (q ? 'No blocked people match.' : 'Nobody is blocked.') + '</span></div>';
            return;
        }
        container.innerHTML = '<div class="ui-group">' + filtered.map(function(b) {
            var note = b.is_blackhole_pending
                ? 'Network block starts at their next announce.'
                : (b.is_network_blocked ? 'Also blocked on the network.' : '');
            return '<div class="block-list-row" data-hash="' + escapeHtml(b.hash) + '">' + RS.kit.avatar(b.hash, 'row') +
                '<span class="block-list-row-text"><b>' + escapeHtml(nameOf(b)) + '</b>' +
                RS.kit.hash.copy(b.hash, { noun: 'LXMF address' }) +
                (note ? '<small>' + escapeHtml(note) + '</small>' : '') + '</span>' +
                '<button type="button" class="ui-btn ui-btn--sm" data-unblock="' + escapeHtml(b.hash) + '" aria-label="Unblock ' + escapeHtml(nameOf(b)) + '">Unblock</button></div>';
        }).join('') + '</div>';
    }

    container.addEventListener('click', function(e) {
        var btn = e.target.closest && e.target.closest('[data-unblock]');
        if (!btn) return;
        var h = btn.getAttribute('data-unblock');
        var entry = allBlocked.find(function(b) { return b.hash === h; }) || { hash: h };
        var displayName = nameOf(entry);
        function done() {
            allBlocked = allBlocked.filter(function(b) { return b.hash !== h; });
            renderBlockList(allBlocked, query());
            updateBlockedCount();
            showToast('Unblocked ' + displayName, 'toast-success', 2500);
        }
        if ((entry.is_network_blocked || entry.is_blackhole_pending) && typeof rsConfirmWithCheckbox === 'function') {
            rsConfirmWithCheckbox({
                message: 'Unblock ' + displayName + '?',
                confirmText: 'Unblock',
                checkboxLabel: 'Also remove the network block',
                checkboxHelp: entry.is_blackhole_pending
                    ? 'Removes the queued network block, which hasn’t started yet. Clear this to leave it queued.'
                    : 'Stops dropping their packets on the network. Clear this to keep the network block.',
                defaultChecked: true
            }).then(function(result) {
                if (!result.confirmed) return;
                RS.invoke('unblock_contact', { args: { hash: h, also_remove_blackhole: result.checked } }).then(done).catch(function() {
                    showToast('Could not unblock ' + displayName, 'toast-error', 3000);
                });
            });
            return;
        }
        RS.invoke('unblock_contact', { args: { hash: h } }).then(done).catch(function() {
            showToast('Could not unblock ' + displayName, 'toast-error', 3000);
        });
    });
}

document.addEventListener('DOMContentLoaded', function() {
    var row = document.getElementById('settings-blocked-count');
    if (row) row.addEventListener('click', openBlockListModal);
    updateBlockedCount();
    // About: policies open in the app; the web links open in the browser.
    document.querySelectorAll('#panel-settings-about [data-about-document]').forEach(function(btn) {
        btn.addEventListener('click', function() {
            if (RS.legal && typeof RS.legal.open === 'function') RS.legal.open(btn.getAttribute('data-about-document'));
        });
    });
    document.querySelectorAll('#panel-settings-about [data-external-url]').forEach(function(btn) {
        btn.addEventListener('click', function() {
            RS.openExternalUrl(btn.getAttribute('data-external-url')).catch(function() {
                showToast('Could not open the link', 'toast-error', 3000);
            });
        });
    });
});

function updateTrustedLinksButton(hosts) {
    var value = document.getElementById('settings-trusted-links-value');
    if (!value) return;
    var n = hosts.length;
    value.textContent = n ? String(n) : 'None';
}

function openTrustedLinksSheet() {
    var built = _rsBuildSheet({ title: 'Trusted websites' }, function() {});
    built.sheet.classList.add('trusted-links-sheet');
    built.overlay.addEventListener('click', function(e) {
        if (e.target === built.overlay) built.dismiss();
    });
    var list = document.createElement('div');
    list.className = 'trusted-links-list';
    built.body.appendChild(list);
    var clearBtn = document.createElement('button');
    clearBtn.type = 'button';
    clearBtn.className = 'rs-dialog-cancel trusted-links-clear';
    clearBtn.textContent = 'Remove All';
    built.footer.appendChild(clearBtn);
    var doneBtn = document.createElement('button');
    doneBtn.type = 'button';
    doneBtn.className = 'rs-dialog-confirm';
    doneBtn.textContent = 'Done';
    doneBtn.addEventListener('click', function() { built.dismiss(); });
    built.footer.appendChild(doneBtn);
    built.sheet.addEventListener('keydown', function(e) {
        if (e.key === 'Escape') { e.stopPropagation(); built.dismiss(); }
        if (e.key === 'Tab') {
            var focusable = Array.prototype.filter.call(built.sheet.querySelectorAll('button'), function(b) { return !b.hidden; });
            if (!focusable.length) return;
            var first = focusable[0], last = focusable[focusable.length - 1];
            if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
            else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
        }
    });

    function render(hosts) {
        updateTrustedLinksButton(hosts);
        list.textContent = '';
        clearBtn.hidden = hosts.length === 0;
        if (!hosts.length) {
            var empty = document.createElement('p');
            empty.className = 'trusted-links-empty';
            empty.textContent = 'No trusted websites. When you open a link, you can choose not to be asked again for that website.';
            list.appendChild(empty);
            return;
        }
        hosts.forEach(function(host) {
            var row = document.createElement('div');
            row.className = 'trusted-links-row';
            var name = document.createElement('span');
            name.className = 'trusted-links-host';
            name.textContent = host;
            var remove = document.createElement('button');
            remove.type = 'button';
            remove.className = 'selector-badge selector-badge-no-caret';
            remove.textContent = 'Remove';
            remove.setAttribute('aria-label', 'Remove ' + host);
            remove.addEventListener('click', function() {
                RS.linkTrust.remove(host).then(function(next) {
                    render(next);
                    showToast('Removed ' + host, 'toast-success', 1800);
                }).catch(function() {
                    showToast('Could not remove ' + host, 'toast-error', 2500);
                });
            });
            row.appendChild(name);
            row.appendChild(remove);
            list.appendChild(row);
        });
    }

    clearBtn.addEventListener('click', function() {
        rsConfirm({
            title: 'Remove all trusted websites?',
            message: 'Links to these websites will ask again before opening.',
            confirmText: 'Remove All',
            danger: true,
        }).then(function(ok) {
            if (!ok) return;
            RS.linkTrust.clear().then(render).catch(function() {
                showToast('Could not remove websites', 'toast-error', 2500);
            });
        });
    });

    render(RS.linkTrust.list());
    built.present();
    doneBtn.focus();
    RS.linkTrust.load().then(render);
}

document.addEventListener('DOMContentLoaded', function() {
    var btn = document.getElementById('settings-trusted-links-btn');
    if (!btn || !(RS.linkTrust)) return;
    btn.addEventListener('click', openTrustedLinksSheet);
    RS.linkTrust.load().then(updateTrustedLinksButton);
});

RS.listen('contact_blocked', function() { updateBlockedCount(); });
RS.listen('contact_unblocked', function() { updateBlockedCount(); });
// Block-list modal listens for `blackhole_update` itself (line 822) so the
// "pending" pill swaps for the active shield in place when the announce-handler
// promotes a queued entry. Here we only refresh the count badge.
RS.listen('blackhole_promoted', function() { updateBlockedCount(); });
