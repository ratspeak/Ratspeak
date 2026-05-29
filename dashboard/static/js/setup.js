var needsSetup = false;

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
                if (typeof showHwUnlock === 'function') showHwUnlock(data.hw_locked);
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

function completeSetupAfterIdentityImport() {
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

    // The imported identity is already active when setup has no identity.
    // Restart the core so the dashboard opens on the imported session.
    RS.invoke('api_setup_restart').catch(function() {});
    runConnectingProgress();
}

window.completeSetupAfterIdentityImport = completeSetupAfterIdentityImport;

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
                    setSetupStep(2);
                    document.getElementById('setup-lxmf-hash').textContent =
                        data.lxmf_hash || data.identity_hash || '--';
                    transitionStep(genStep, document.getElementById('setup-step-2'), null, function() {
                        var nameInput = document.getElementById('setup-display-name');
                        if (nameInput && !isMobile()) nameInput.focus();
                    });
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
