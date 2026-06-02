var needsSetup = false;
var SETUP_RECOVERY_PHRASE_WORDS = 12;
var setupRecoveryMnemonic = '';

function setSetupStep(stepIndex) {
    var dots = document.querySelectorAll('#setup-progress-dots .setup-dot');
    for (var i = 0; i < dots.length; i++) {
        dots[i].classList.toggle('active', i === stepIndex);
    }
}

function transitionStep(hideEl, showEl, extras, callback) {
    var FADE_OUT = 200;
    var FADE_IN = 280;
    if (hideEl) {
        hideEl.classList.add('setup-fade-out');
    }
    if (extras) {
        for (var i = 0; i < extras.length; i++) {
            if (extras[i]) extras[i].classList.add('setup-fade-out');
        }
    }
    setTimeout(function() {
        if (hideEl) { hideEl.style.display = 'none'; hideEl.classList.remove('setup-fade-out'); }
        if (extras) {
            for (var i = 0; i < extras.length; i++) {
                if (extras[i]) { extras[i].style.display = 'none'; extras[i].classList.remove('setup-fade-out'); }
            }
        }
        if (showEl) {
            showEl.style.display = 'block';
            showEl.classList.add('setup-fade-in');
            setTimeout(function() {
                showEl.classList.remove('setup-fade-in');
                if (callback) callback();
            }, FADE_IN);
        } else {
            if (callback) callback();
        }
    }, FADE_OUT);
}

function checkSetupStatus() {
    RS.invoke('api_setup_status')
        .then(function(data) {
            if (data.needs_setup) {
                needsSetup = true;
                enterSetupMode();
            } else {
                document.body.classList.remove('setup-active');
            }
            document.body.classList.remove('checking-setup');
        })
        .catch(function() {
            document.body.classList.remove('checking-setup');
        });
}

function enterSetupMode() {
    document.body.classList.add('setup-active');

    var sidebar = document.querySelector('.sidebar');
    if (sidebar) sidebar.style.display = 'none';

    document.querySelectorAll('.view').forEach(function(v) {
        v.classList.remove('active');
    });

    var setupView = document.getElementById('view-setup');
    if (setupView) setupView.classList.add('active');

    var headerRight = document.querySelector('.header-right');
    if (headerRight) headerRight.style.display = 'none';

    var bottomBar = document.getElementById('bottom-bar');
    if (bottomBar) bottomBar.style.display = 'none';

    var uptimeEl = document.getElementById('uptime');
    if (uptimeEl) uptimeEl.style.display = 'none';

    ['header-mobile-name', 'header-mobile-status'].forEach(function(id) {
        var el = document.getElementById(id);
        if (el) el.textContent = '';
    });
    var hdrAvatar = document.getElementById('header-mobile-avatar');
    if (hdrAvatar) hdrAvatar.innerHTML = '';
}

function startCryptoAnimation() {
    var el = document.getElementById('crypto-hex-stream');
    if (el) el.style.display = 'none';

    var step1 = document.getElementById('prog-step-1');
    var step2 = document.getElementById('prog-step-2');
    if (step1) step1.classList.remove('done');
    if (step2) step2.classList.remove('active', 'done');
    if (step1) step1.classList.add('active');

    function randomDelay() {
        return 675 + Math.floor(Math.random() * 676);
    }

    return new Promise(function(resolve) {
        setTimeout(function() {
            if (step1) { step1.classList.remove('active'); step1.classList.add('done'); }
            if (step2) step2.classList.add('active');

            setTimeout(function() {
                if (step2) { step2.classList.remove('active'); step2.classList.add('done'); }
                var lockIcon = document.getElementById('crypto-lock-icon');
                if (lockIcon) lockIcon.classList.add('locked');
                resolve();
            }, randomDelay());
        }, randomDelay());
    });
}

function setupRecoveryWords(mnemonic) {
    if (typeof window.recoveryPhraseWords === 'function') {
        return window.recoveryPhraseWords(mnemonic);
    }
    return String(mnemonic || '').trim().split(/\s+/).filter(Boolean);
}

function setupRecoveryPhraseGrid(words) {
    if (typeof window.renderRecoveryPhraseGrid === 'function') {
        return window.renderRecoveryPhraseGrid(words);
    }
    return (words || []).map(function(word, i) {
        return '<div class="hw-mnemonic-word">' +
            '<span class="hw-mnemonic-index">' + (i + 1) + '</span>' +
            '<span class="hw-mnemonic-text">' + escapeHtml(word) + '</span>' +
        '</div>';
    }).join('');
}

function resetSetupRecoveryStep() {
    setupRecoveryMnemonic = '';
    var grid = document.getElementById('setup-mnemonic-grid');
    if (grid) {
        grid.innerHTML = '';
        grid.setAttribute('aria-hidden', 'true');
    }
    var shell = document.getElementById('setup-mnemonic-shell');
    if (shell) shell.classList.remove('revealed');
    var cover = document.getElementById('setup-mnemonic-cover');
    if (cover) cover.style.display = '';
    var confirm = document.getElementById('setup-mnemonic-confirm');
    if (confirm) confirm.checked = false;
    var continueBtn = document.getElementById('setup-mnemonic-continue-btn');
    if (continueBtn) continueBtn.disabled = true;
}

function showSetupRecoveryStep(mnemonic, fromEl) {
    var words = setupRecoveryWords(mnemonic);
    if (words.length !== SETUP_RECOVERY_PHRASE_WORDS) {
        showSetupIdentityStep(fromEl);
        return;
    }

    setupRecoveryMnemonic = mnemonic;
    var grid = document.getElementById('setup-mnemonic-grid');
    if (grid) {
        grid.innerHTML = setupRecoveryPhraseGrid(words);
        grid.setAttribute('aria-hidden', 'true');
    }

    var shell = document.getElementById('setup-mnemonic-shell');
    if (shell) shell.classList.remove('revealed');
    var cover = document.getElementById('setup-mnemonic-cover');
    if (cover) cover.style.display = '';
    var confirm = document.getElementById('setup-mnemonic-confirm');
    if (confirm) confirm.checked = false;
    var continueBtn = document.getElementById('setup-mnemonic-continue-btn');
    if (continueBtn) continueBtn.disabled = true;

    setSetupStep(2);
    transitionStep(fromEl, document.getElementById('setup-step-backup'), null, function() {
        if (cover && !isMobile()) cover.focus();
    });
}

function showSetupIdentityStep(fromEl) {
    setSetupStep(3);
    transitionStep(fromEl, document.getElementById('setup-step-2'), null, function() {
        var nameInput = document.getElementById('setup-display-name');
        if (nameInput && !isMobile()) nameInput.focus();
    });
}

function runConnectingProgress() {
    var pollDone = false;
    var maxPolls = 60;
    var pollAttempt = 0;
    var startedAt = Date.now();

    function onServerReady() {
        if (pollDone) return;
        pollDone = true;

        var elapsed = Date.now() - startedAt;
        var remaining = Math.max(0, 2000 - elapsed);
        setTimeout(function() {
            window.location.href = '/#dashboard';
            window.location.reload();
        }, remaining);
    }

    // After a soft reset, first wait for IPC to answer, then poll startup state.
    function pollAlive() {
        pollAttempt++;
        RS.invoke('api_version').then(function() {
            pollProgress();
        }).catch(function() {
            if (pollAttempt < maxPolls) {
                setTimeout(pollAlive, 1000);
            } else {
                onServerReady();
            }
        });
    }

    function pollProgress() {
        if (pollDone) return;
        pollAttempt++;
        RS.invoke('api_startup_progress').then(function(data) {
            if (data.stage === 'hw_locked') {
                // Active identity is a locked hardware key — prompt for the PIN.
                // Unlock re-inits the runtime and reloads the app.
                if (typeof showHwUnlock === 'function') showHwUnlock(data.hw_locked, data.hw_locked_kind);
                return;
            }
            if (data.stage === 'ready') {
                onServerReady();
            } else if (pollAttempt < maxPolls) {
                setTimeout(pollProgress, 500);
            } else {
                onServerReady();
            }
        }).catch(function() {
            if (pollAttempt < maxPolls) {
                setTimeout(pollAlive, 1000);
            } else {
                onServerReady();
            }
        });
    }

    pollAlive();
}

function showSetupConnectingStep() {
    needsSetup = false;
    setSetupStep(3);

    var headerExtras = [
        document.querySelector('.setup-icon'),
        document.querySelector('.setup-title'),
        document.querySelector('.setup-subtitle'),
        document.getElementById('setup-progress-dots')
    ];
    var visibleStep = null;
    document.querySelectorAll('#view-setup .setup-step').forEach(function(step) {
        if (!visibleStep && step.style.display !== 'none') visibleStep = step;
    });
    transitionStep(
        visibleStep || document.getElementById('setup-step-1'),
        document.getElementById('setup-step-connecting'),
        headerExtras
    );
}

function resetSetupToStart() {
    needsSetup = true;
    document.body.classList.add('setup-active');
    setSetupStep(0);
    resetSetupRecoveryStep();
    [
        document.querySelector('.setup-icon'),
        document.querySelector('.setup-title'),
        document.querySelector('.setup-subtitle'),
        document.getElementById('setup-progress-dots')
    ].forEach(function(el) {
        if (el) {
            el.style.display = '';
            el.classList.remove('setup-fade-out');
        }
    });
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
    RS.invoke('api_setup_restart').catch(function() {});
    runConnectingProgress();
}

window.completeSetupAfterIdentityImport = completeSetupAfterIdentityImport;

function completeSetupAfterHardwareIdentity(result, pin) {
    result = result || {};
    var hash = result.hash || '';
    showSetupConnectingStep();
    if (!hash || !pin) {
        resetSetupToStart();
        if (typeof showToast === 'function') {
            showToast('Hardware setup did not return an identity to unlock.', 'toast-red', 6000);
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
                    if (typeof showToast === 'function') showToast(detail, 'toast-red', 7000);
                    window.RS.diag('error', '[setup] Hardware identity unlock failed:', detail);
                    return;
                }
                RS.invoke('hw_remove', { hash: hash }).catch(function() {}).finally(function() {
                    resetSetupToStart();
                    if (typeof showToast === 'function') showToast('Security key reset. Set up a new identity or restore from your recovery phrase.', 'toast-green', 7000);
                });
            });
            return;
        }
        resetSetupToStart();
        if (typeof showToast === 'function') showToast(detail, 'toast-red', 7000);
        window.RS.diag('error', '[setup] Hardware identity unlock failed:', detail);
    }

    RS.invoke('hw_activate_and_unlock', { hash: hash, pin: pin }).then(function(res) {
        if (res && res.ok) {
            runConnectingProgress();
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

// Mobile tap-toggle for .tooltip-trigger; desktop uses CSS hover/focus.
function initSetupTooltips() {
    if (!isMobile()) return;
    var triggers = document.querySelectorAll('#view-setup .tooltip-trigger');
    if (!triggers.length) return;

    var backdrop = document.querySelector('.tooltip-backdrop');
    if (!backdrop) {
        backdrop = document.createElement('div');
        backdrop.className = 'tooltip-backdrop';
        document.body.appendChild(backdrop);
    }

    var open = null;
    function close() {
        if (!open) return;
        open.classList.remove('open');
        backdrop.classList.remove('open');
        open = null;
    }

    triggers.forEach(function(t) {
        t.addEventListener('click', function(e) {
            e.preventDefault();
            e.stopPropagation();
            if (open === t) { close(); return; }
            close();
            t.classList.add('open');
            backdrop.classList.add('open');
            open = t;
        });
    });

    backdrop.addEventListener('click', close);
    document.addEventListener('keydown', function(e) {
        if (e.key === 'Escape') close();
    });
}

document.addEventListener('DOMContentLoaded', function() {

    initSetupTooltips();

    var generateBtn = document.getElementById('setup-generate-btn');
    if (generateBtn) {
        generateBtn.addEventListener('click', function() {
            generateBtn.disabled = true;
            generateBtn.textContent = 'Creating...';

            setSetupStep(1);
            var step1El = document.getElementById('setup-step-1');
            var genStep = document.getElementById('setup-step-generating');
            transitionStep(step1El, genStep, null, function() {});

            var animPromise = startCryptoAnimation();

            var backendPromise = RS.invoke('api_setup_complete', { args: { display_name: '' } });

            Promise.all([backendPromise, animPromise])
                .then(function(results) {
                    var data = results[0] || {};
                    if (data.ok === false) {
                        throw new Error(data.error || 'Failed to create identity');
                    }
                    document.getElementById('setup-lxmf-hash').textContent =
                        data.lxmf_hash || data.identity_hash || '--';
                    showSetupRecoveryStep(data.mnemonic || '', genStep);
                })
                .catch(function(err) {
                    transitionStep(genStep, document.getElementById('setup-step-1'));
                    generateBtn.disabled = false;
                    generateBtn.textContent = 'Create Identity';
                    var detail = err ? (err.message || String(err)) : 'Unknown error';
                    window.RS.diag('error', '[setup] Create identity failed:', detail);
                    if (typeof showToast === 'function') {
                        showToast('Request failed: ' + detail, 'toast-red', 5000);
                    }
                });
        });
    }

    var setupMnemonicCover = document.getElementById('setup-mnemonic-cover');
    if (setupMnemonicCover) {
        setupMnemonicCover.addEventListener('click', function() {
            setupMnemonicCover.style.display = 'none';
            var shell = document.getElementById('setup-mnemonic-shell');
            if (shell) shell.classList.add('revealed');
            var grid = document.getElementById('setup-mnemonic-grid');
            if (grid) grid.setAttribute('aria-hidden', 'false');
        });
    }

    var setupMnemonicCopy = document.getElementById('setup-mnemonic-copy-btn');
    if (setupMnemonicCopy) {
        setupMnemonicCopy.addEventListener('click', function() {
            if (!setupRecoveryMnemonic) return;
            if (!navigator.clipboard) {
                if (typeof showToast === 'function') showToast('Clipboard is not available', 'toast-orange', 2000);
                return;
            }
            navigator.clipboard.writeText(setupRecoveryMnemonic).then(function() {
                if (typeof showCopyConfirmationToast === 'function') {
                    showCopyConfirmationToast('Recovery phrase');
                }
            }).catch(function() {
                if (typeof showToast === 'function') showToast('Could not copy phrase', 'toast-orange', 2000);
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
        if ((typeof isMobile === 'function') && isMobile()) {
            hardwareKeyBtn.style.display = 'none';
        }
        hardwareKeyBtn.addEventListener('click', function() {
            if (typeof openHardwareWizard === 'function') openHardwareWizard({ fromSetup: true });
        });
    }

    var copyBtn = document.getElementById('setup-copy-btn');
    if (copyBtn) {
        copyBtn.addEventListener('click', function() {
            var hashEl = document.getElementById('setup-lxmf-hash');
            var hashText = hashEl ? hashEl.textContent : '';
            if (hashText && hashText !== '\u2014' && hashText !== '--') {
                navigator.clipboard.writeText(hashText).then(function() {
                    if (typeof showCopyConfirmationToast === 'function') {
                        showCopyConfirmationToast('Address');
                    }
                }).catch(function() {
                    // Clipboard API unavailable; fall back to text selection.
                    var range = document.createRange();
                    range.selectNodeContents(hashEl);
                    var sel = window.getSelection();
                    sel.removeAllRanges();
                    sel.addRange(range);
                });
            }
        });
    }

    var finishBtn = document.getElementById('setup-finish-btn');
    if (finishBtn) {
        finishBtn.addEventListener('click', function() {
            var displayName = document.getElementById('setup-display-name').value.trim();
            finishBtn.disabled = true;
            finishBtn.textContent = 'Connecting...';

            RS.invoke('api_setup_complete', { args: { display_name: displayName || '' } })
            .then(function() {
                setSetupStep(3);
                var headerExtras = [
                    document.querySelector('.setup-icon'),
                    document.querySelector('.setup-title'),
                    document.querySelector('.setup-subtitle'),
                    document.getElementById('setup-progress-dots')
                ];
                transitionStep(
                    document.getElementById('setup-step-2'),
                    document.getElementById('setup-step-connecting'),
                    headerExtras
                );

                // Core may restart mid-request before responding.
                RS.invoke('api_setup_restart').catch(function() {});

                runConnectingProgress();
            })
            .catch(function() {
                finishBtn.disabled = false;
                finishBtn.textContent = 'Connect';
                if (typeof showToast === 'function') {
                    showToast('Failed to complete setup', 'toast-red', 5000);
                }
            });
        });
    }

    var setupNameInput = document.getElementById('setup-display-name');
    if (setupNameInput && finishBtn) {
        setupNameInput.addEventListener('keydown', function(e) {
            if (e.key === 'Enter') {
                e.preventDefault();
                if (!finishBtn.disabled) finishBtn.click();
            }
        });
    }
});

checkSetupStatus();
