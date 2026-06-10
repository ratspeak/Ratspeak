// Replaces browser-native confirm()/prompt(); all dialogs use the bottom-sheet shell.

function rsConfirm(opts) {
    opts = opts || {};
    return new Promise(function(resolve) {
        _rsShowDialog({
            title: opts.title || 'Confirm',
            message: opts.message || 'Are you sure?',
            confirmText: opts.confirmText || 'Confirm',
            cancelText: opts.cancelText || 'Cancel',
            danger: !!opts.danger,
            hasInput: false
        }, function(confirmed) {
            resolve(confirmed);
        });
    });
}

function rsAlert(opts) {
    opts = opts || {};
    return new Promise(function(resolve) {
        var built = _rsBuildSheet({ title: opts.title || 'Ratspeak' }, function() {
            resolve();
        });

        built.overlay.addEventListener('click', function(e) {
            if (e.target === built.overlay) built.dismiss(null);
        });

        var message = document.createElement('div');
        message.className = 'rs-dialog-message';
        message.textContent = opts.message || '';
        built.body.appendChild(message);

        var closeBtn = document.createElement('button');
        closeBtn.className = 'rs-dialog-confirm';
        closeBtn.textContent = opts.closeText || 'Close';
        closeBtn.addEventListener('click', function() {
            built.dismiss(null);
        });
        built.footer.appendChild(closeBtn);

        built.sheet.addEventListener('keydown', function(e) {
            if (e.key === 'Escape') {
                e.stopPropagation();
                built.dismiss(null);
            }
            if (e.key === 'Enter') {
                e.preventDefault();
                built.dismiss(null);
            }
            if (e.key === 'Tab') {
                var focusable = built.sheet.querySelectorAll('button');
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

        RS.gestures.attachDragDismiss(built.sheet, {
            axis: 'y',
            blockIfScrolled: true,
            skipIf: function(e) {
                return !!(e.target.closest('button') || e.target.tagName === 'INPUT');
            },
            parallaxOverlay: built.overlay,
            onCommit: function() { built.dismiss(null); }
        });

        built.present();
        closeBtn.focus();
    });
}

// Resolves { confirmed, checked }.
function rsConfirmWithCheckbox(opts) {
    opts = opts || {};
    return new Promise(function(resolve) {
        _rsShowDialog({
            title: opts.title || 'Confirm',
            message: opts.message || 'Are you sure?',
            confirmText: opts.confirmText || 'Confirm',
            cancelText: opts.cancelText || 'Cancel',
            danger: !!opts.danger,
            hasInput: false,
            checkbox: {
                label: opts.checkboxLabel || '',
                help: opts.checkboxHelp || '',
                defaultChecked: !!opts.defaultChecked
            }
        }, function(result) {
            resolve(result || { confirmed: false, checked: false });
        });
    });
}

// Resolves the input string on confirm, null on cancel.
function rsPrompt(opts) {
    opts = opts || {};
    return new Promise(function(resolve) {
        _rsShowDialog({
            title: opts.title || 'Input',
            message: opts.message || '',
            confirmText: opts.confirmText || 'OK',
            cancelText: opts.cancelText || 'Cancel',
            danger: false,
            hasInput: true,
            placeholder: opts.placeholder || '',
            defaultValue: opts.defaultValue || ''
        }, function(value) {
            resolve(value);
        });
    });
}

// Builds canonical .bottom-sheet shell. Caller wires its own overlay-click
// handler so dismiss-on-tap can use live state.
function _rsBuildSheet(opts, onClose) {
    var resolved = false;
    var previousFocus = document.activeElement;

    var shell = RS.sheetShell.create();
    var overlay = shell.overlay;
    // Stack above any open sheet — iOS mid-animation sheets bury confirmations otherwise.
    overlay.style.zIndex = '99999';

    var sheet = shell.sheet;
    sheet.setAttribute('role', 'dialog');
    sheet.setAttribute('aria-modal', 'true');
    sheet.style.zIndex = '100000';

    var handle = document.createElement('div');
    handle.className = 'bottom-sheet-handle';
    sheet.appendChild(handle);

    if (opts.title) {
        var header = document.createElement('div');
        header.className = 'bottom-sheet-header';
        var title = document.createElement('div');
        title.className = 'bottom-sheet-title';
        if (opts.titleIcon) {
            title.classList.add('bottom-sheet-title-with-icon');
            if (opts.titleIconType) title.dataset.sheetIcon = opts.titleIconType;

            var titleIcon = document.createElement('span');
            titleIcon.className = 'bottom-sheet-title-icon';
            titleIcon.innerHTML = opts.titleIcon;
            title.appendChild(titleIcon);

            var titleLabel = document.createElement('span');
            titleLabel.className = 'bottom-sheet-title-label';
            titleLabel.textContent = opts.title;
            title.appendChild(titleLabel);
        } else {
            title.textContent = opts.title;
        }
        header.appendChild(title);
        sheet.appendChild(header);
    }

    var body = document.createElement('div');
    body.className = 'bottom-sheet-body';
    sheet.appendChild(body);

    var footer = document.createElement('div');
    footer.className = 'bottom-sheet-footer';
    sheet.appendChild(footer);

    function dismiss(value) {
        if (resolved) return;
        resolved = true;

        RS.sheetShell.dismiss(shell, function() {
            if (previousFocus && previousFocus.focus) previousFocus.focus();
        });

        if (onClose) onClose(value);
    }

    function present() {
        RS.sheetShell.present(shell);
    }

    return {
        overlay: overlay,
        sheet: sheet,
        body: body,
        footer: footer,
        present: present,
        dismiss: dismiss
    };
}

function _rsShowDialog(cfg, callback) {
    var input = null;
    var checkbox = null;

    function resolveValue(confirmed) {
        if (cfg.checkbox) {
            return { confirmed: !!confirmed, checked: !!(checkbox && checkbox.checked) };
        }
        if (cfg.hasInput) {
            return confirmed ? input.value : null;
        }
        return !!confirmed;
    }

    var built = _rsBuildSheet({ title: cfg.title }, callback);

    built.overlay.addEventListener('click', function(e) {
        if (e.target === built.overlay) built.dismiss(resolveValue(false));
    });

    var message = document.createElement('div');
    message.className = 'rs-dialog-message';
    message.textContent = cfg.message;
    built.body.appendChild(message);

    if (cfg.hasInput) {
        input = document.createElement('input');
        input.type = 'text';
        input.className = 'rs-dialog-input';
        input.placeholder = cfg.placeholder;
        input.value = cfg.defaultValue;
        disableAutoCorrect(input);
        built.body.appendChild(input);
    }

    if (cfg.checkbox) {
        var checkboxWrap = document.createElement('label');
        checkboxWrap.className = 'rs-dialog-checkbox-wrap';
        checkbox = document.createElement('input');
        checkbox.type = 'checkbox';
        checkbox.className = 'rs-dialog-checkbox';
        checkbox.checked = !!cfg.checkbox.defaultChecked;
        var labelSpan = document.createElement('span');
        labelSpan.className = 'rs-dialog-checkbox-label';
        labelSpan.textContent = cfg.checkbox.label || '';
        checkboxWrap.appendChild(checkbox);
        checkboxWrap.appendChild(labelSpan);
        if (cfg.checkbox.help) {
            var helpDiv = document.createElement('div');
            helpDiv.className = 'rs-dialog-checkbox-help';
            helpDiv.textContent = cfg.checkbox.help;
            checkboxWrap.appendChild(helpDiv);
        }
        built.body.appendChild(checkboxWrap);
    }

    var cancelBtn = document.createElement('button');
    cancelBtn.className = 'rs-dialog-cancel';
    cancelBtn.textContent = cfg.cancelText;
    cancelBtn.addEventListener('click', function() {
        built.dismiss(resolveValue(false));
    });

    var confirmBtn = document.createElement('button');
    confirmBtn.className = 'rs-dialog-confirm' + (cfg.danger ? ' rs-dialog-danger' : '');
    confirmBtn.textContent = cfg.confirmText;
    confirmBtn.addEventListener('click', function() {
        built.dismiss(resolveValue(true));
    });

    built.footer.appendChild(cancelBtn);
    built.footer.appendChild(confirmBtn);

    built.sheet.addEventListener('keydown', function(e) {
        if (e.key === 'Escape') {
            e.stopPropagation();
            built.dismiss(resolveValue(false));
        }
        if (e.key === 'Enter' && (!cfg.hasInput || document.activeElement === input)) {
            e.preventDefault();
            built.dismiss(resolveValue(true));
        }
        if (e.key === 'Tab') {
            var focusable = built.sheet.querySelectorAll('input, button');
            if (focusable.length === 0) return;
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

    RS.gestures.attachDragDismiss(built.sheet, {
        axis: 'y',
        blockIfScrolled: true,
        skipIf: function(e) {
            if (cfg.hasInput && document.activeElement === input) return true;
            return !!(e.target.closest('button') || e.target.tagName === 'INPUT');
        },
        parallaxOverlay: built.overlay,
        onCommit: function() { built.dismiss(resolveValue(false)); }
    });

    built.present();

    if (input) {
        if (!isMobile()) { input.focus(); input.select(); }
    } else {
        confirmBtn.focus();
    }
}

// Resolves the chosen value or null on cancel.
function rsChoice(opts) {
    opts = opts || {};
    return new Promise(function(resolve) {
        var built = _rsBuildSheet({
            title: opts.title || 'Choose',
            titleIcon: opts.titleIcon || '',
            titleIconType: opts.titleIconType || ''
        }, resolve);

        built.overlay.addEventListener('click', function(e) {
            if (e.target === built.overlay) built.dismiss(null);
        });

        if (opts.message) {
            var msg = document.createElement('div');
            msg.className = 'rs-dialog-message';
            msg.textContent = opts.message;
            built.body.appendChild(msg);
        }

        var choicesWrap = document.createElement('div');
        choicesWrap.className = 'rs-dialog-choices';
        (opts.choices || []).forEach(function(choice) {
            var btn = document.createElement('button');
            btn.type = 'button';
            btn.className = 'rs-dialog-choice' + (choice.danger ? ' rs-dialog-danger' : '');

            if (choice.icon) {
                var icon = document.createElement('span');
                icon.className = 'rs-dialog-choice-icon';
                icon.innerHTML = choice.icon;
                btn.appendChild(icon);
            }

            var text = document.createElement('span');
            text.className = 'rs-dialog-choice-text';
            var label = document.createElement('span');
            label.className = 'rs-dialog-choice-label';
            label.textContent = choice.label;
            text.appendChild(label);
            if (choice.hint) {
                var hint = document.createElement('span');
                hint.className = 'rs-dialog-choice-hint';
                hint.textContent = choice.hint;
                text.appendChild(hint);
            }
            btn.appendChild(text);
            btn.addEventListener('click', function() { built.dismiss(choice.value); });
            choicesWrap.appendChild(btn);
        });
        built.body.appendChild(choicesWrap);

        if (opts.cancelText !== false) {
            var cancelBtn = document.createElement('button');
            cancelBtn.className = 'rs-dialog-cancel';
            cancelBtn.textContent = opts.cancelText || 'Cancel';
            cancelBtn.addEventListener('click', function() { built.dismiss(null); });
            built.footer.appendChild(cancelBtn);
        }

        built.sheet.addEventListener('keydown', function(e) {
            if (e.key === 'Escape') { e.stopPropagation(); built.dismiss(null); }
            if (e.key === 'Tab') {
                var focusable = built.sheet.querySelectorAll('button');
                if (!focusable.length) return;
                var first = focusable[0], last = focusable[focusable.length - 1];
                if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
                else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
            }
        });

        RS.gestures.attachDragDismiss(built.sheet, {
            axis: 'y',
            blockIfScrolled: true,
            skipIf: function(e) {
                return !!(e.target.closest('button') || e.target.tagName === 'INPUT');
            },
            parallaxOverlay: built.overlay,
            onCommit: function() { built.dismiss(null); }
        });

        built.present();

        var firstBtn = choicesWrap.querySelector('button');
        if (firstBtn) firstBtn.focus();
    });
}

function rsPromptContact(opts) {
    opts = opts || {};
    return new Promise(function(resolve) {
        var built = _rsBuildSheet({ title: opts.title || 'Add Contact' }, resolve);

        built.overlay.addEventListener('click', function(e) {
            if (e.target === built.overlay) built.dismiss(null);
        });

        var nameLabel = document.createElement('label');
        nameLabel.className = 'rs-dialog-field-label';
        nameLabel.textContent = 'Name (optional)';

        var nameInput = document.createElement('input');
        nameInput.type = 'text';
        nameInput.className = 'rs-dialog-input';
        nameInput.placeholder = 'Display name';
        nameInput.maxLength = 64;
        disableAutoCorrect(nameInput);

        var hashLabel = document.createElement('label');
        hashLabel.className = 'rs-dialog-field-label';
        hashLabel.textContent = 'Identity hash';

        var hashInput = document.createElement('input');
        hashInput.type = 'text';
        hashInput.className = 'rs-dialog-input';
        hashInput.placeholder = '16+ hex characters';
        hashInput.value = opts.defaultHash || '';
        disableAutoCorrect(hashInput);

        var hashError = document.createElement('div');
        hashError.className = 'rs-dialog-field-error';
        hashError.style.display = 'none';

        built.body.appendChild(nameLabel);
        built.body.appendChild(nameInput);
        built.body.appendChild(hashLabel);
        built.body.appendChild(hashInput);
        built.body.appendChild(hashError);

        var cancelBtn = document.createElement('button');
        cancelBtn.className = 'rs-dialog-cancel';
        cancelBtn.textContent = opts.cancelText || 'Cancel';
        cancelBtn.addEventListener('click', function() { built.dismiss(null); });

        var confirmBtn = document.createElement('button');
        confirmBtn.className = 'rs-dialog-confirm';
        confirmBtn.textContent = opts.confirmText || 'Add';

        function attemptConfirm() {
            var hash = hashInput.value.trim();
            if (hash.length < 16 || !/^[0-9a-fA-F]+$/.test(hash)) {
                hashError.textContent = 'Must be 16+ hexadecimal characters';
                hashError.style.display = '';
                hashInput.focus();
                hashInput.select();
                return;
            }
            built.dismiss({ hash: hash, display_name: nameInput.value.trim() });
        }
        confirmBtn.addEventListener('click', attemptConfirm);

        hashInput.addEventListener('input', function() {
            if (hashError.style.display !== 'none') hashError.style.display = 'none';
        });

        built.footer.appendChild(cancelBtn);
        built.footer.appendChild(confirmBtn);

        built.sheet.addEventListener('keydown', function(e) {
            if (e.key === 'Escape') { e.stopPropagation(); built.dismiss(null); }
            if (e.key === 'Enter') {
                if (document.activeElement === nameInput) {
                    e.preventDefault();
                    hashInput.focus();
                    return;
                }
                if (document.activeElement === hashInput) {
                    e.preventDefault();
                    attemptConfirm();
                }
            }
            if (e.key === 'Tab') {
                var focusable = built.sheet.querySelectorAll('input, button');
                if (!focusable.length) return;
                var first = focusable[0], last = focusable[focusable.length - 1];
                if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
                else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
            }
        });

        RS.gestures.attachDragDismiss(built.sheet, {
            axis: 'y',
            blockIfScrolled: true,
            skipIf: function(e) {
                if (document.activeElement === nameInput || document.activeElement === hashInput) return true;
                return !!(e.target.closest('button') || e.target.tagName === 'INPUT');
            },
            parallaxOverlay: built.overlay,
            onCommit: function() { built.dismiss(null); }
        });

        built.present();

        if (!isMobile()) {
            nameInput.focus();
        }
    });
}

// Returns a controller { update, success, error, close, isOpen, onClose }.
function rsProgress(opts) {
    opts = opts || {};
    var resolved = false;
    var previousFocus = document.activeElement;
    var _onClose = null;

    var built = _rsBuildSheet({ title: opts.title || 'Working...' }, function() {
        resolved = true;
        if (_onClose) _onClose();
    });
    built.sheet.setAttribute('aria-live', 'polite');
    // Not user-dismissible — overlay-tap intentionally not wired.

    var bodyInner = document.createElement('div');
    bodyInner.className = 'rs-progress-body';

    var spinner = document.createElement('span');
    spinner.className = 'loading-spinner';

    var statusText = document.createElement('span');
    statusText.className = 'rs-progress-text';
    statusText.textContent = opts.message || '';

    bodyInner.appendChild(spinner);
    bodyInner.appendChild(statusText);
    built.body.appendChild(bodyInner);

    built.footer.style.display = 'none';

    var _timeoutId = null;

    if (typeof opts.onCancel === 'function') {
        var cancelBtn = document.createElement('button');
        cancelBtn.className = 'rs-dialog-cancel';
        cancelBtn.textContent = opts.cancelText || 'Cancel';
        cancelBtn.addEventListener('click', function() {
            try { opts.onCancel(); } catch (_) {}
            close();
        });
        built.footer.style.display = '';
        built.footer.appendChild(cancelBtn);
    }

    function close() {
        if (resolved) return;
        if (_timeoutId) clearTimeout(_timeoutId);
        built.dismiss(null);
    }

    var _errored = false;
    function showError(text) {
        if (_errored || resolved) return;
        _errored = true;
        if (_timeoutId) { clearTimeout(_timeoutId); _timeoutId = null; }
        spinner.className = 'rs-progress-icon rs-progress-error';
        spinner.textContent = '✗';
        statusText.textContent = text;
        bodyInner.classList.add('rs-progress-done');
        // Replace any prior footer buttons so only Close remains.
        built.footer.innerHTML = '';
        var closeBtn = document.createElement('button');
        closeBtn.className = 'rs-dialog-confirm';
        closeBtn.textContent = 'Close';
        closeBtn.addEventListener('click', close);
        built.footer.style.display = '';
        built.footer.appendChild(closeBtn);
        closeBtn.focus();
    }

    if (opts.timeout && opts.timeout > 0) {
        _timeoutId = setTimeout(function() {
            if (!resolved) {
                showError(opts.timeoutMessage || 'Operation timed out');
            }
        }, opts.timeout);
    }

    built.present();

    return {
        update: function(text) { statusText.textContent = text; },
        success: function(text) {
            spinner.className = 'rs-progress-icon rs-progress-success';
            spinner.textContent = '✓';
            statusText.textContent = text;
            bodyInner.classList.add('rs-progress-done');
            setTimeout(close, 1500);
        },
        error: showError,
        close: close,
        isOpen: function() { return !resolved; },
        onClose: function(fn) { _onClose = fn; }
    };
}
