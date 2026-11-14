var needsSetup = false;
var SETUP_RECOVERY_PHRASE_WORDS = 12;
var setupRecoveryMnemonic = '';

// The last row of a progress list holds until this long after its screen
// appeared, so a fast start never just flashes. A minimum, never an addition.
var SETUP_PROGRESS_HOLD_MS = 900;
var SETUP_START_POLL_MS = 150;
var SETUP_START_GIVE_UP_MS = 60000;

// The app asks for notification permission once per install by itself:
// during first run, or on the first launch of an install that predates the
// ask. Every later ask is the person's, from Settings.
var SETUP_NOTIFICATION_ASK_KEY = 'ratspeak_notification_prompt_settled';

function _setupNotificationAskSettled() {
    if (typeof _mobileNotificationPermissionWasRequested === 'function' &&
            _mobileNotificationPermissionWasRequested()) return true;
    try { return localStorage.getItem(SETUP_NOTIFICATION_ASK_KEY) === '1'; }
    catch (_) { return false; }
}

// Resolves once the request is issued, not answered, so startup never waits on it.
function requestNotificationPermissionOnce() {
    if (typeof isTauriMobile !== 'function' || !isTauriMobile()) return Promise.resolve(false);
    if (typeof rsNotify === 'undefined' || !rsNotify.available()) return Promise.resolve(false);
    if (_setupNotificationAskSettled()) return Promise.resolve(false);
    try { localStorage.setItem(SETUP_NOTIFICATION_ASK_KEY, '1'); } catch (_) {}
    return RS.invoke('api_notification_settings').then(function(data) {
        if (!data || data.enabled !== true) return false;
        rsNotify.setEnabled(true);
        return rsNotify.permissionState().then(function(state) {
            if (state !== 'prompt') return false;
            rsNotify.requestPermission();
            return true;
        });
    }).catch(function() { return false; });
}

function dismissSetupKeyboard() {
    var input = document.getElementById('setup-display-name');
    if (input && typeof input.blur === 'function') input.blur();
}

function transitionStep(hideEl, showEl, callback) {
    var FADE_OUT = 200;
    var FADE_IN = 280;
    if (hideEl) hideEl.classList.add('setup-fade-out');
    setTimeout(function() {
        if (hideEl) { hideEl.style.display = 'none'; hideEl.classList.remove('setup-fade-out'); }
        if (showEl) {
            showEl.style.display = 'block';
            showEl.classList.add('setup-fade-in');
            var title = showEl.querySelector('.solo-title');
            if (title && !isMobile()) title.focus({ preventScroll: true });
            setTimeout(function() {
                showEl.classList.remove('setup-fade-in');
                if (callback) callback();
            }, FADE_IN);
        } else if (callback) {
            callback();
        }
    }, hideEl ? FADE_OUT : 0);
}

function visibleSetupStep() {
    var visible = null;
    document.querySelectorAll('#view-setup .setup-step').forEach(function(step) {
        if (!visible && step.style.display !== 'none') visible = step;
    });
    return visible;
}

function checkSetupStatus() {
    RS.invoke('api_setup_status')
        .then(function(data) {
            if (data.needs_setup) {
                needsSetup = true;
                enterSetupMode();
            } else {
                document.body.classList.remove('setup-active');
                // Installs from before the one-time ask get it once, here.
                requestNotificationPermissionOnce();
            }
            document.body.classList.remove('checking-setup');
        })
        .catch(function(err) {
            // A failed start keeps the app hidden behind its startup screen.
            if (!err || err.code !== 'startup_error') document.body.classList.remove('checking-setup');
        });
}

function enterSetupMode() {
    document.body.classList.add('setup-active');

    var sidebar = document.getElementById('sidebar');
    if (sidebar) sidebar.style.display = 'none';

    document.querySelectorAll('.view').forEach(function(v) {
        v.classList.remove('active');
    });

    var setupView = document.getElementById('view-setup');
    if (setupView) setupView.classList.add('active');

    var bottomBar = document.getElementById('bottom-bar');
    if (bottomBar) bottomBar.style.display = 'none';

    var uptimeEl = document.getElementById('uptime');
    if (uptimeEl) uptimeEl.style.display = 'none';
}

// ---- Progress rows ----

function setSetupProgressStep(id, status) {
    var row = document.getElementById(id);
    if (!row) return;
    row.className = 'progress-step' + (status && status !== 'pending' ? ' is-' + status : '');
    var meta = row.querySelector('.progress-step-meta');
    if (meta) meta.textContent = status === 'done' ? 'Done' : status === 'failed' ? 'Stopped' : '';
}

function announceSetupProgress(id, text) {
    var live = document.getElementById(id);
    if (live) live.textContent = text;
}

// Keys and address come back in one reply: the keys tick when it arrives, the
// address holds to the minimum.
function runGenerateProgress(backendPromise) {
    var shownAt = Date.now();
    setSetupProgressStep('setup-gen-keys', 'active');
    setSetupProgressStep('setup-gen-address', 'pending');
    announceSetupProgress('setup-generate-live', '');
    return backendPromise.then(function(result) {
        setSetupProgressStep('setup-gen-keys', 'done');
        setSetupProgressStep('setup-gen-address', 'active');
        announceSetupProgress('setup-generate-live', 'Generating your keys: done.');
        var wait = Math.max(0, shownAt + SETUP_PROGRESS_HOLD_MS - Date.now());
        return new Promise(function(resolve) {
            setTimeout(function() {
                setSetupProgressStep('setup-gen-address', 'done');
                resolve(result);
            }, wait);
        });
    });
}

// ---- Recovery phrase ----

function setupRecoveryWords(mnemonic) {
    if (typeof window.recoveryPhraseWords === 'function') {
        return window.recoveryPhraseWords(mnemonic);
    }
    return String(mnemonic || '').trim().split(/\s+/).filter(Boolean);
}

// Covered, the well holds stand-ins; the words exist in the page only once shown.
function renderSetupPhrase(words) {
    var grid = document.getElementById('setup-mnemonic-grid');
    if (!grid) return;
    var shown = !!words;
    var items = [];
    for (var i = 0; i < SETUP_RECOVERY_PHRASE_WORDS; i++) {
        items.push('<li>' + (shown ? escapeHtml(words[i] || '') : 'xxxxxx') + '</li>');
    }
    grid.innerHTML = items.join('');
    grid.setAttribute('aria-hidden', shown ? 'false' : 'true');
    var shell = document.getElementById('setup-mnemonic-shell');
    if (shell) shell.classList.toggle('is-covered', !shown);
    var cover = document.getElementById('setup-mnemonic-cover');
    if (cover) cover.hidden = shown;
    var copy = document.getElementById('setup-mnemonic-copy-btn');
    if (copy) copy.disabled = !shown;
    var confirm = document.getElementById('setup-mnemonic-confirm');
    if (confirm) {
        confirm.disabled = !shown;
        if (!shown) confirm.checked = false;
    }
    var continueBtn = document.getElementById('setup-mnemonic-continue-btn');
    if (continueBtn) continueBtn.disabled = !(shown && confirm && confirm.checked);
}

function resetSetupRecoveryStep() {
    setupRecoveryMnemonic = '';
    renderSetupPhrase(null);
}

function showSetupRecoveryStep(mnemonic, fromEl) {
    var words = setupRecoveryWords(mnemonic);
    if (words.length !== SETUP_RECOVERY_PHRASE_WORDS) {
        showSetupIdentityStep(fromEl);
        return;
    }
    setupRecoveryMnemonic = mnemonic;
    renderSetupPhrase(null);
    transitionStep(fromEl, document.getElementById('setup-step-backup'), function() {
        var cover = document.getElementById('setup-mnemonic-cover');
        if (cover && !isMobile()) cover.focus();
    });
}

function showSetupIdentityStep(fromEl) {
    transitionStep(fromEl, document.getElementById('setup-step-2'), function() {
        var nameInput = document.getElementById('setup-display-name');
        if (nameInput && !isMobile()) nameInput.focus();
    });
}

function renderSetupAddress(hash) {
    var el = document.getElementById('setup-lxmf-hash');
    if (!el) return;
    if (hash && RS.kit && RS.kit.hash) {
        el.innerHTML = RS.kit.hash.copy(hash, { form: 'full', noun: 'LXMF address' });
    } else {
        el.textContent = hash || '—';
    }
}

// ---- Starting Ratspeak ----

// Startup timing rule. Each real step ticks only when the runtime reaches it:
// the identity is loaded once the stage passes "rns", and the network is up at
// "ready". The playful step then holds until SETUP_PROGRESS_HOLD_MS after the
// screen appeared, and adds nothing when the work took longer.
var SETUP_START_STAGE_RANK = { lxmf: 1, rns: 2, ready: 3 };

function createSetupStartTimeline(shownAt) {
    return {
        shownAt: shownAt,
        network: 'active',
        identity: 'pending',
        nest: 'pending',
        leaveAt: null,
        finished: false,
        failed: null
    };
}

function advanceSetupStartTimeline(t, stage, now) {
    if (t.finished || t.failed) return t;
    if (stage === 'error') {
        // The step that was running stops; a step that never got going is pending again.
        t.failed = t.identity === 'active' ? 'identity' : 'network';
        if (t.failed === 'identity') {
            t.identity = 'failed';
            if (t.network === 'active') t.network = 'pending';
        } else {
            t.network = 'failed';
        }
        t.nest = 'pending';
        return t;
    }
    var rank = SETUP_START_STAGE_RANK[stage] || 0;
    if (rank >= 1 && t.identity === 'pending') t.identity = 'active';
    if (rank >= 2) t.identity = 'done';
    if (rank >= 3) {
        t.network = 'done';
        if (t.nest === 'pending') {
            t.nest = 'active';
            t.leaveAt = Math.max(now, t.shownAt + SETUP_PROGRESS_HOLD_MS);
        }
    }
    return tickSetupStartTimeline(t, now);
}

function tickSetupStartTimeline(t, now) {
    if (t.nest === 'active' && t.leaveAt !== null && now >= t.leaveAt) {
        t.nest = 'done';
        t.finished = true;
    }
    return t;
}

function setupStartFailureLead(step, detail) {
    var text = String(detail || '');
    var reason;
    if (step === 'identity') {
        reason = 'Ratspeak couldn’t load your identity.';
    } else if (/already in use|os error (98|48|10048)\b|address in use/i.test(text)) {
        reason = 'The network didn’t start because another app is using its port.';
    } else {
        reason = 'The network didn’t start.';
    }
    return reason + ' Your identity is saved, so nothing is lost.';
}

var _setupStartRun = null;
var _setupStartDetail = '';
var _setupStartVisibleAt = 0;

function renderSetupStart(t) {
    setSetupProgressStep('setup-start-network', t.network);
    setSetupProgressStep('setup-start-identity', t.identity);
    setSetupProgressStep('setup-start-nest', t.nest);
    var steps = document.getElementById('setup-start-steps');
    if (steps) steps.classList.toggle('is-running', !t.failed);
    var done = [];
    if (t.network === 'done') done.push('Starting the network: done.');
    if (t.identity === 'done') done.push('Importing identity: done.');
    announceSetupProgress('setup-start-live', done.join(' '));
}

function resetSetupStartScreen() {
    var title = document.getElementById('setup-start-title');
    if (title) title.textContent = 'Starting Ratspeak';
    var lead = document.getElementById('setup-start-lead');
    if (lead) lead.textContent = 'Getting everything ready. This takes a moment.';
    var actions = document.getElementById('setup-start-actions');
    if (actions) actions.hidden = true;
    announceSetupProgress('setup-start-alert', '');
    _setupStartDetail = '';
    renderSetupStart(createSetupStartTimeline(Date.now()));
}

function showSetupConnectingStep() {
    needsSetup = false;
    resetSetupStartScreen();
    var step = document.getElementById('setup-step-connecting');
    var from = visibleSetupStep();
    if (from === step) return;
    // The hold counts from when the card is on screen, after the fade out.
    _setupStartVisibleAt = Date.now() + (from ? 200 : 0);
    transitionStep(from, step);
}

function stopSetupStartRun() {
    if (!_setupStartRun) return;
    clearTimeout(_setupStartRun.timer);
    _setupStartRun = null;
}

function finishSetupStart() {
    stopSetupStartRun();
    window.location.href = '/#' + setupCompletionView();
    window.location.reload();
}

function failSetupStart(t) {
    stopSetupStartRun();
    RS.invoke('api_network_ownership').catch(function() { return null; }).then(function(status) {
        var networkError = status && status.error ? String(status.error) : '';
        _setupStartDetail = 'Ratspeak couldn’t start.\nStep: ' +
            (t.failed === 'identity' ? 'Importing identity' : 'Starting the network') +
            '\nStartup stage: error' + (networkError ? '\nNetwork: ' + networkError : '');
        var title = document.getElementById('setup-start-title');
        if (title) title.textContent = 'Ratspeak couldn’t start';
        var lead = document.getElementById('setup-start-lead');
        var leadText = setupStartFailureLead(t.failed, networkError);
        if (lead) lead.textContent = leadText;
        var actions = document.getElementById('setup-start-actions');
        if (actions) actions.hidden = false;
        announceSetupProgress('setup-start-alert', 'Ratspeak couldn’t start. ' + leadText);
        if (title) title.focus({ preventScroll: true });
    });
}

// Polls the runtime's startup stage. Call only once the stage belongs to this
// start: after api_setup_restart replies, or once an unlock has finished.
function runConnectingProgress() {
    stopSetupStartRun();
    var t = createSetupStartTimeline(_setupStartVisibleAt || Date.now());
    var run = _setupStartRun = { timeline: t, timer: null };
    renderSetupStart(t);

    function schedule(fn, ms) {
        if (_setupStartRun !== run) return;
        clearTimeout(run.timer);
        run.timer = setTimeout(fn, ms);
    }

    function settle() {
        if (_setupStartRun !== run) return;
        var now = Date.now();
        tickSetupStartTimeline(t, now);
        renderSetupStart(t);
        if (t.finished) { finishSetupStart(); return; }
        if (t.nest === 'active') { schedule(settle, Math.max(0, t.leaveAt - now)); return; }
        // A start that never reports opens the app, as before.
        if (now - t.shownAt >= SETUP_START_GIVE_UP_MS) { finishSetupStart(); return; }
        schedule(poll, SETUP_START_POLL_MS);
    }

    function poll() {
        if (_setupStartRun !== run) return;
        RS.invoke('api_startup_progress').then(function(data) {
            if (_setupStartRun !== run) return;
            if (data && data.stage === 'hw_locked') {
                // Active identity is locked — prompt for the PIN. Unlock
                // re-inits the runtime and reloads the app.
                stopSetupStartRun();
                if (typeof showHwUnlock === 'function') showHwUnlock(data.hw_locked, data.hw_locked_kind);
                return;
            }
            advanceSetupStartTimeline(t, data && data.stage, Date.now());
            if (t.failed) { renderSetupStart(t); failSetupStart(t); return; }
            settle();
        }).catch(function() {
            // The core can be between sessions for a moment.
            schedule(poll, 500);
        });
    }

    poll();
}

// The one-time ask goes out before the restart, so the restart cannot drop it.
function restartSetupCore() {
    return requestNotificationPermissionOnce().then(function() {
        return RS.invoke('api_setup_restart').catch(function() {});
    }).then(runConnectingProgress);
}

function setupCompletionView() {
    if (typeof appLandingView === 'function') return appLandingView();
    var nativeMobile = typeof isTauriMobile === 'function' && isTauriMobile();
    var compact = typeof isCompactLayout === 'function' && isCompactLayout();
    return nativeMobile || compact ? 'peers' : 'dashboard';
}

function resetSetupToStart() {
    needsSetup = true;
    stopSetupStartRun();
    document.body.classList.add('setup-active');
    resetSetupRecoveryStep();
    resetSetupStartScreen();
    document.querySelectorAll('#view-setup .setup-step').forEach(function(step) {
        step.style.display = 'none';
        step.classList.remove('setup-fade-out', 'setup-fade-in');
    });
    var first = document.getElementById('setup-step-1');
    if (first) first.style.display = 'block';
}

function completeSetupAfterIdentityImport() {
    showSetupConnectingStep();
    // The imported identity is already active when setup has no identity.
    // Restart the core so the dashboard opens on the imported session.
    restartSetupCore();
}

window.completeSetupAfterIdentityImport = completeSetupAfterIdentityImport;

function completeSetupAfterHardwareIdentity(result, pin) {
    result = result || {};
    var hash = result.hash || '';
    showSetupConnectingStep();
    if (!hash || !pin) {
        resetSetupToStart();
        if (typeof showToast === 'function') {
            showToast('Hardware setup did not provide an identity. Try setup again.', 'toast-error', 6000);
        }
        return;
    }

    function isPinLockedMessage(message) {
        message = String(message || '').toLowerCase();
        return message.indexOf('pin is locked') >= 0 ||
            message.indexOf('pin locked') >= 0 ||
            message.indexOf('requires puk') >= 0 ||
            message.indexOf('requires both retry counters') >= 0 ||
            message.indexOf('reset the piv application') >= 0;
    }

    function failHardwareUnlock(detail, locked) {
        if ((locked || isPinLockedMessage(detail)) && typeof window.resetHardwarePivWithConfirmation === 'function') {
            resetSetupToStart();
            window.resetHardwarePivWithConfirmation({
                title: 'Reset security key?',
                message: 'The YubiKey PIN is locked. Ratspeak can reset the key’s PIV application and return to setup. This erases the Ratspeak identity keys on this YubiKey, but does not affect passkeys, FIDO sign-ins, OTP, or other non-PIV features.'
            }).then(function(reset) {
                if (!reset) {
                    if (typeof showToast === 'function') showToast(detail, 'toast-error', 7000);
                    window.RS.diag('error', '[setup] Hardware identity unlock failed:', detail);
                    return;
                }
                RS.invoke('hw_remove', { hash: hash }).catch(function() {}).finally(function() {
                    resetSetupToStart();
                    if (typeof showToast === 'function') showToast('Security key reset. Create or restore an identity to continue.', 'toast-success', 7000);
                });
            });
            return;
        }
        resetSetupToStart();
        if (typeof showToast === 'function') showToast(detail, 'toast-error', 7000);
        window.RS.diag('error', '[setup] Hardware identity unlock failed:', detail);
    }

    // A failed activation also ends with the switch's closing event. It carries
    // this reply's message, so in either order the toast shows it once.
    RS.invoke('hw_activate_and_unlock', { hash: hash, pin: pin }).then(function(res) {
        if (res && res.ok) {
            // The unlock has already loaded the identity; the stage is final.
            requestNotificationPermissionOnce().then(runConnectingProgress);
            return;
        }
        var detail = (res && res.error) ? res.error : 'Could not unlock the hardware identity.';
        failHardwareUnlock(detail, !!(res && res.locked));
    }).catch(function(err) {
        var detail = (err && err.message) ? err.message : 'Could not unlock the hardware identity.';
        failHardwareUnlock(detail, false);
    });
}

window.completeSetupAfterHardwareIdentity = completeSetupAfterHardwareIdentity;

function initSetupTooltips() {
    if (RS.ui && typeof RS.ui.bindHelpPopovers === 'function') {
        RS.ui.bindHelpPopovers(document.getElementById('view-setup'));
    }
}

function setSetupButtonBusy(button, label) {
    if (!button) return;
    button.disabled = !!label;
    button.setAttribute('aria-busy', label ? 'true' : 'false');
    if (label) {
        button.dataset.idleLabel = button.dataset.idleLabel || button.textContent;
        button.innerHTML = '<span class="ui-spin" aria-hidden="true"></span>' + escapeHtml(label);
    } else if (button.dataset.idleLabel) {
        button.textContent = button.dataset.idleLabel;
    }
}

document.addEventListener('DOMContentLoaded', function() {

    initSetupTooltips();

    var generateBtn = document.getElementById('setup-generate-btn');
    if (generateBtn) {
        generateBtn.addEventListener('click', function() {
            generateBtn.disabled = true;
            var step1El = document.getElementById('setup-step-1');
            var genStep = document.getElementById('setup-step-generating');
            transitionStep(step1El, genStep);

            var backendPromise = RS.invoke('api_setup_complete', { args: { display_name: '' } })
                .then(function(data) {
                    data = data || {};
                    if (data.ok === false) {
                        throw new Error(data.error || 'Failed to create identity');
                    }
                    return data;
                });

            runGenerateProgress(backendPromise)
                .then(function(data) {
                    renderSetupAddress(data.lxmf_hash || data.identity_hash || '');
                    showSetupRecoveryStep(data.mnemonic || '', genStep);
                })
                .catch(function(err) {
                    transitionStep(genStep, document.getElementById('setup-step-1'));
                    generateBtn.disabled = false;
                    var detail = err ? (err.message || String(err)) : 'Unknown error';
                    window.RS.diag('error', '[setup] Create identity failed:', detail);
                    if (typeof showToast === 'function') {
                        showToast('Could not complete the request: ' + detail, 'toast-error', 5000);
                    }
                });
        });
    }

    var setupMnemonicCover = document.getElementById('setup-mnemonic-cover');
    if (setupMnemonicCover) {
        setupMnemonicCover.addEventListener('click', function() {
            if (!setupRecoveryMnemonic) return;
            renderSetupPhrase(setupRecoveryWords(setupRecoveryMnemonic));
            // The cover button is gone; keep keyboard focus in the step.
            var copy = document.getElementById('setup-mnemonic-copy-btn');
            if (copy && RS.ui && typeof RS.ui.prefersKeyboardFocus === 'function' && RS.ui.prefersKeyboardFocus()) {
                copy.focus({ preventScroll: true });
            }
        });
    }

    var setupMnemonicCopy = document.getElementById('setup-mnemonic-copy-btn');
    if (setupMnemonicCopy) {
        setupMnemonicCopy.addEventListener('click', function() {
            if (!setupRecoveryMnemonic) return;
            RS.copySecret(setupRecoveryMnemonic).then(function(ok) {
                if (ok) {
                    if (typeof showSecretCopiedToast === 'function') {
                        showSecretCopiedToast('Recovery phrase');
                    }
                } else if (typeof showToast === 'function') {
                    showToast('Could not copy the recovery phrase', 'toast-error', 2000);
                }
            });
        });
    }

    var setupMnemonicConfirm = document.getElementById('setup-mnemonic-confirm');
    if (setupMnemonicConfirm) {
        setupMnemonicConfirm.addEventListener('change', function() {
            var continueBtn = document.getElementById('setup-mnemonic-continue-btn');
            if (continueBtn) continueBtn.disabled = !setupMnemonicConfirm.checked;
        });
    }

    var setupMnemonicContinue = document.getElementById('setup-mnemonic-continue-btn');
    if (setupMnemonicContinue) {
        setupMnemonicContinue.addEventListener('click', function() {
            if (setupMnemonicContinue.disabled) return;
            showSetupIdentityStep(document.getElementById('setup-step-backup'));
        });
    }

    var importBtn = document.getElementById('setup-import-identity-btn');
    if (importBtn) {
        importBtn.addEventListener('click', function() {
            window._identityImportFromSetup = true;
            if (typeof importIdentity === 'function') importIdentity();
        });
    }

    var hardwareKeyBtn = document.getElementById('setup-hardware-key-btn');
    if (hardwareKeyBtn) {
        // Hardware (YubiKey/PIV) identities are desktop-only for now — the
        // `hardware` feature + hw_* commands are gated off on mobile.
        // TODO(ratkey-mobile): mobile needs the wrapped-session model (tap to
        // unlock a software session via on-card ECDH) — see HARDWARE_STATUS.md.
        if ((typeof supportsHardwareIdentities === 'function') && !supportsHardwareIdentities()) {
            hardwareKeyBtn.style.display = 'none';
        }
        hardwareKeyBtn.addEventListener('click', function() {
            if (typeof openHardwareWizard === 'function') openHardwareWizard({ fromSetup: true });
        });
    }

    var finishBtn = document.getElementById('setup-finish-btn');
    if (finishBtn) {
        finishBtn.addEventListener('click', function() {
            dismissSetupKeyboard();
            var displayName = document.getElementById('setup-display-name').value.trim();
            setSetupButtonBusy(finishBtn, 'Starting…');

            RS.invoke('api_setup_complete', { args: { display_name: displayName || '' } })
            .then(function() {
                showSetupConnectingStep();
                restartSetupCore();
            })
            .catch(function() {
                setSetupButtonBusy(finishBtn, null);
                if (typeof showToast === 'function') {
                    showToast('Could not complete setup', 'toast-error', 5000);
                }
            });
        });
    }

    var setupNameInput = document.getElementById('setup-display-name');
    if (setupNameInput && finishBtn) {
        setupNameInput.addEventListener('keydown', function(e) {
            if (e.key === 'Enter' && !e.isComposing) {
                e.preventDefault();
                if (!finishBtn.disabled) finishBtn.click();
            }
        });
    }

    var retryBtn = document.getElementById('setup-retry-btn');
    if (retryBtn) {
        retryBtn.addEventListener('click', function() {
            resetSetupStartScreen();
            _setupStartVisibleAt = Date.now();
            var title = document.getElementById('setup-start-title');
            if (title && !isMobile()) title.focus({ preventScroll: true });
            RS.invoke('api_setup_restart').catch(function() {}).then(runConnectingProgress);
        });
    }

    var copyDetailsBtn = document.getElementById('setup-copy-details-btn');
    if (copyDetailsBtn) {
        copyDetailsBtn.addEventListener('click', function() {
            if (!_setupStartDetail) return;
            Promise.resolve(RS.copyText(_setupStartDetail)).then(function(ok) {
                if (!ok) {
                    if (typeof showToast === 'function') showToast('Could not copy details', 'toast-error', 3000);
                    return;
                }
                copyDetailsBtn.textContent = 'Copied';
                clearTimeout(copyDetailsBtn._rsCopiedTimer);
                copyDetailsBtn._rsCopiedTimer = setTimeout(function() {
                    copyDetailsBtn.textContent = 'Copy details';
                }, 2000);
            });
        });
    }
});

checkSetupStatus();
