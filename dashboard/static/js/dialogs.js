// Replaces browser-native confirm()/prompt(). Every runtime dialog is the kit dialog: centred on
// desktop, a bottom sheet on a phone; Small, Medium ('md'), or Large ('lg').

function rsConfirm(opts) {
    opts = opts || {};
    return new Promise(function(resolve) {
        _rsShowDialog({
            title: opts.title || 'Confirm',
            message: opts.message || '',
            confirmText: opts.confirmText || 'Confirm',
            cancelText: opts.cancelText || 'Cancel',
            danger: !!opts.danger,
            initialFocus: opts.initialFocus,
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

        built.body.appendChild(_rsText('p', '', opts.message || ''));

        var closeBtn = _rsButton(opts.closeText || 'Close', 'primary');
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
            if (e.key === 'Tab') _rsTrapTab(e, built.sheet, 'button');
        });

        _rsAttachDrag(built, function() { built.dismiss(null); });

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
            message: opts.message || '',
            confirmText: opts.confirmText || 'Confirm',
            cancelText: opts.cancelText || 'Cancel',
            danger: !!opts.danger,
            initialFocus: opts.initialFocus,
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
            confirmText: opts.confirmText || 'Save',
            cancelText: opts.cancelText || 'Cancel',
            danger: false,
            hasInput: true,
            label: opts.label || '',
            placeholder: opts.placeholder || '',
            defaultValue: opts.defaultValue || '',
            maxLength: opts.maxLength || 0,
            inputClass: opts.inputClass || ''
        }, function(value) {
            resolve(value);
        });
    });
}

function _rsHandleNativeBack(opts, dismiss) {
    if (opts.nativeBackDismissible === false) return false;
    var value = typeof opts.nativeBackValue === 'function'
        ? opts.nativeBackValue()
        : (Object.prototype.hasOwnProperty.call(opts, 'nativeBackValue')
            ? opts.nativeBackValue
            : null);
    dismiss(value);
    return true;
}

function _rsText(tag, className, text) {
    var el = document.createElement(tag);
    if (className) el.className = className;
    el.textContent = text;
    return el;
}

// kind: 'primary', 'danger' (a destructive dialog's confirm), 'quiet', or 'cancel' (Quiet; a phone
// sheet drops it, since the handle, the scrim, and Back already close it).
function _rsButton(label, kind) {
    var btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'ui-btn' + ({
        primary: ' ui-btn--primary',
        danger: ' ui-btn--danger ui-btn--primary',
        quiet: ' ui-btn--quiet',
        cancel: ' ui-btn--quiet ui-desk-only'
    }[kind] || '');
    btn.textContent = label;
    return btn;
}

var _rsFieldSeq = 0;
function _rsField(labelText, control, help) {
    var field = document.createElement('div');
    field.className = 'ui-field';
    if (!control.id) control.id = 'rs-dialog-field-' + (++_rsFieldSeq);
    if (labelText) {
        var label = _rsText('label', 'ui-label', labelText);
        label.htmlFor = control.id;
        field.appendChild(label);
    }
    field.appendChild(control);
    if (help) {
        var helpEl = _rsText('p', 'ui-help', help);
        helpEl.id = control.id + '-help';
        control.setAttribute('aria-describedby', helpEl.id);
        field.appendChild(helpEl);
    }
    return field;
}

function _rsCheckRow(labelText, checked) {
    var row = document.createElement('label');
    row.className = 'ui-check-row';
    var input = document.createElement('input');
    input.type = 'checkbox';
    input.checked = !!checked;
    var box = document.createElement('span');
    box.className = 'ui-check';
    box.innerHTML = RS.kit.icon('check');
    row.appendChild(input);
    row.appendChild(box);
    row.appendChild(_rsText('span', '', labelText));
    return { row: row, input: input };
}

function _rsTrapTab(e, sheet, selector) {
    var focusable = Array.prototype.filter.call(sheet.querySelectorAll(selector), function(el) {
        return !el.disabled && el.offsetParent !== null;
    });
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

function _rsAttachDrag(built, onCommit, skip) {
    RS.gestures.attachDragDismiss(built.sheet, {
        axis: 'y',
        blockIfScrolled: true,
        skipIf: function(e) {
            if (skip && skip(e)) return true;
            return !!(e.target.closest('button') || e.target.tagName === 'INPUT');
        },
        parallaxOverlay: built.overlay,
        onCommit: onCommit
    });
}

var RS_DIALOG_SIZES = { md: ' ui-dialog--md', lg: ' ui-dialog--lg' };
// Connection sheets keep their type's tone on the title tile.
var RS_DIALOG_TILE_TONES = { tcp: 'info', backbone: 'info', host: 'neutral', lora: 'online', local: 'warning', ble: 'purple' };
var _rsDialogSeq = 0;

// Builds the kit dialog: .ui-modal (the scrim) > .ui-dialog.rs-sheet-shell. Callers wire their own
// scrim click so dismiss-on-tap can use live state. opts: title, size ('md' | 'lg'), sheetClass,
// titleIcon (markup) and titleIconType, showTitle, ariaLabel, role, nativeBackValue, nativeBackDismissible.
function _rsBuildSheet(opts, onClose) {
    opts = opts || {};
    var resolved = false;
    var previousFocus = document.activeElement;

    var overlay = document.createElement('div');
    overlay.className = 'ui-modal';
    // Stack above any open sheet — iOS mid-animation sheets bury confirmations otherwise.
    overlay.style.zIndex = '99999';

    var sheet = document.createElement('div');
    sheet.className = 'ui-dialog rs-sheet-shell' + (RS_DIALOG_SIZES[opts.size] || '') +
        (opts.sheetClass ? ' ' + opts.sheetClass : '');
    sheet.setAttribute('role', opts.role || 'dialog');
    sheet.setAttribute('aria-modal', 'true');
    overlay.appendChild(sheet);

    var handle = document.createElement('div');
    handle.className = 'ui-dialog-handle';
    handle.setAttribute('aria-hidden', 'true');
    sheet.appendChild(handle);

    var head = document.createElement('div');
    head.className = 'ui-dialog-head';
    var title = null;
    if (opts.title && opts.showTitle !== false) {
        if (opts.titleIcon) {
            var tile = document.createElement('span');
            tile.className = 'ui-tile';
            if (RS_DIALOG_TILE_TONES[opts.titleIconType]) tile.classList.add('ui-tile--' + RS_DIALOG_TILE_TONES[opts.titleIconType]);
            tile.setAttribute('aria-hidden', 'true');
            tile.innerHTML = opts.titleIcon;
            var glyph = tile.querySelector('svg');
            if (glyph) glyph.classList.add('ui-ico');
            head.appendChild(tile);
        }
        title = _rsText('h2', 'ui-dialog-title', opts.title);
        title.id = 'rs-dialog-title-' + (++_rsDialogSeq);
        head.appendChild(title);
        sheet.setAttribute('aria-labelledby', title.id);
    } else {
        sheet.setAttribute('aria-label', opts.ariaLabel || opts.title || 'Dialog');
    }
    if (opts.nativeBackDismissible !== false) {
        var close = document.createElement('button');
        close.type = 'button';
        close.className = 'ui-icon-btn';
        close.setAttribute('aria-label', 'Close');
        close.title = 'Close';
        close.innerHTML = RS.kit.icon('x');
        close.addEventListener('click', function() {
            if (typeof sheet._ratspeakDismiss === 'function') sheet._ratspeakDismiss();
        });
        if (!title) close.classList.add('rs-dialog-close');
        head.appendChild(close);
    }
    if (head.firstChild) sheet.appendChild(head);

    var body = document.createElement('div');
    body.className = 'ui-dialog-body';
    body.setAttribute('data-sheet-scroll', '');
    sheet.appendChild(body);

    var footer = document.createElement('div');
    footer.className = 'ui-dialog-foot';
    sheet.appendChild(footer);

    function dismiss(value) {
        if (resolved) return;
        resolved = true;
        sheet.classList.remove('open');
        overlay.classList.add('is-closing');
        setTimeout(function() {
            if (overlay.parentNode) overlay.remove();
            var restoreFocus = RS.ui && typeof RS.ui.prefersKeyboardFocus === 'function'
                ? RS.ui.prefersKeyboardFocus()
                : true;
            if (restoreFocus && previousFocus && previousFocus.focus) previousFocus.focus();
        }, 200);

        if (onClose) onClose(value);
    }

    // Close, Escape, and native Back share one state-aware close; non-dismissible progress
    // dialogs still consume Back. Callers may replace it.
    sheet._ratspeakDismiss = function() {
        return _rsHandleNativeBack(opts, dismiss);
    };
    overlay.addEventListener('keydown', function(e) {
        if (e.key !== 'Escape' || e.defaultPrevented) return;
        e.preventDefault();
        sheet._ratspeakDismiss();
    });

    function present() {
        document.body.appendChild(overlay);
        sheet.classList.add('open');
    }

    return {
        overlay: overlay,
        sheet: sheet,
        head: head,
        title: title,
        body: body,
        footer: footer,
        present: present,
        dismiss: dismiss
    };
}

function _rsShowDialog(cfg, callback) {
    var input = null;
    var checkbox = null;
    // A destructive confirm starts on Cancel, so Enter never destroys by default.
    var initialFocus = cfg.initialFocus || (cfg.danger ? 'cancel' : 'confirm');

    function resolveValue(confirmed) {
        if (cfg.checkbox) {
            return { confirmed: !!confirmed, checked: !!(checkbox && checkbox.checked) };
        }
        if (cfg.hasInput) {
            return confirmed ? input.value : null;
        }
        return !!confirmed;
    }

    var built = _rsBuildSheet({
        title: cfg.title,
        role: cfg.danger ? 'alertdialog' : 'dialog',
        nativeBackValue: function() { return resolveValue(false); }
    }, callback);

    built.overlay.addEventListener('click', function(e) {
        if (e.target === built.overlay) built.dismiss(resolveValue(false));
    });

    if (cfg.message) {
        var message = _rsText('p', '', cfg.message);
        message.id = (built.title ? built.title.id : 'rs-dialog') + '-message';
        built.sheet.setAttribute('aria-describedby', message.id);
        built.body.appendChild(message);
    }
    if (cfg.detail) built.body.appendChild(cfg.detail);

    if (cfg.hasInput) {
        input = document.createElement('input');
        input.type = 'text';
        input.className = 'ui-input' + (cfg.inputClass ? ' ' + cfg.inputClass : '');
        input.placeholder = cfg.placeholder;
        input.value = cfg.defaultValue;
        if (cfg.maxLength) input.maxLength = cfg.maxLength;
        if (!cfg.label) input.setAttribute('aria-label', cfg.title);
        disableAutoCorrect(input);
        built.body.appendChild(_rsField(cfg.label, input));
    }

    if (cfg.checkbox) {
        var check = _rsCheckRow(cfg.checkbox.label || '', cfg.checkbox.defaultChecked);
        checkbox = check.input;
        if (cfg.checkbox.help) {
            var group = document.createElement('div');
            group.className = 'rs-dialog-check';
            group.appendChild(check.row);
            group.appendChild(_rsText('p', 'ui-help', cfg.checkbox.help));
            built.body.appendChild(group);
        } else {
            built.body.appendChild(check.row);
        }
    }

    var cancelBtn = _rsButton(cfg.cancelText, 'cancel');
    cancelBtn.addEventListener('click', function() {
        built.dismiss(resolveValue(false));
    });

    var confirmBtn = _rsButton(cfg.confirmText, cfg.danger ? 'danger' : 'primary');
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
        if (e.key === 'Enter') {
            var active = document.activeElement;
            // A focused button activates natively; Enter must not override Cancel.
            if (active && active.tagName === 'BUTTON' && built.sheet.contains(active)) return;
            if (cfg.hasInput ? active === input : initialFocus !== 'cancel') {
                e.preventDefault();
                built.dismiss(resolveValue(true));
            }
        }
        if (e.key === 'Tab') _rsTrapTab(e, built.sheet, 'input, button');
    });

    _rsAttachDrag(built, function() { built.dismiss(resolveValue(false)); }, function() {
        return !!(cfg.hasInput && document.activeElement === input);
    });

    built.present();

    if (input) {
        if (!isMobile()) { input.focus(); input.select(); }
    } else if (initialFocus === 'cancel') {
        if (!(RS.ui && typeof RS.ui.prefersKeyboardFocus === 'function') || RS.ui.prefersKeyboardFocus()) {
            cancelBtn.focus();
        }
    } else {
        confirmBtn.focus();
    }
}

var RS_LINK_PATH_CHARS = 24;
function _rsHostOfText(text) {
    var m = /^\s*(?:https?:\/\/)?((?:[a-z0-9-]+\.)+[a-z]{2,})\.?(?:[\/:?#]|\s*$)/i.exec(text || '');
    return m ? m[1].toLowerCase().replace(/^www\./, '') : null;
}

// Display parts and warnings for an outbound link. The host is never
// truncated; punycode stays punycode so look-alike hosts stay visible.
function rsLinkTarget(href, linkText) {
    var url;
    try { url = new URL(href); } catch (_) { return null; }
    if (url.protocol !== 'https:' && url.protocol !== 'http:') return null;
    var host = url.hostname.toLowerCase().replace(/\.$/, '');
    var www = /^www\.[^.]+\./.test(host) ? 'www.' : '';
    var rest = (url.pathname === '/' ? '' : url.pathname) + url.search + url.hash;
    var warnings = [];
    var textHost = _rsHostOfText(linkText);
    var mismatch = linkText != null && textHost && textHost !== host.replace(/^www\./, '');
    if (mismatch) warnings.push('The link text shows ' + textHost + ', but it opens ' + host + '.');
    if (url.username || url.password) {
        warnings.push('This link hides its real address.' + (mismatch ? '' : ' It opens ' + host + '.'));
    }
    if (host.split('.').some(function(label) { return label.indexOf('xn--') === 0; })) {
        warnings.push('This address uses international characters, which can imitate other websites.');
    }
    return {
        href: url.href,
        host: host,
        // www.example.org and example.org share one trust entry.
        trustKey: host.slice(www.length),
        parts: {
            scheme: url.protocol === 'http:' ? 'http://' : '',
            www: www,
            host: host.slice(www.length),
            port: url.port ? ':' + url.port : '',
            rest: rest.length > RS_LINK_PATH_CHARS ? rest.slice(0, RS_LINK_PATH_CHARS) + '\u2026' : rest,
        },
        warnings: warnings,
    };
}

function _rsLinkDetail(target) {
    var wrap = document.createElement('div');
    wrap.className = 'rs-link-detail';
    var box = document.createElement('div');
    box.className = 'ui-well rs-link-target';
    var text = document.createElement('span');
    text.className = 'rs-link-text';
    [['scheme', 'quiet'], ['www', 'quiet'], ['host', 'host'], ['port', 'quiet'], ['rest', 'quiet']].forEach(function(part) {
        if (!target.parts[part[0]]) return;
        text.appendChild(_rsText('span', 'rs-link-' + part[1], target.parts[part[0]]));
    });
    var copy = document.createElement('button');
    copy.type = 'button';
    copy.className = 'ui-icon-btn ui-icon-btn--sm';
    copy.setAttribute('aria-label', 'Copy link');
    copy.title = 'Copy link';
    copy.innerHTML = RS.kit.icon('copy');
    copy.addEventListener('click', function() {
        Promise.resolve(RS.copyText(target.href)).then(function(ok) {
            if (ok === false) showToast('Could not copy the link', 'toast-error', 1800);
            else showToast('Link copied', 'toast-success', 1800);
        }).catch(function() {
            showToast('Could not copy the link', 'toast-error', 1800);
        });
    });
    box.appendChild(text);
    box.appendChild(copy);
    wrap.appendChild(box);
    target.warnings.forEach(function(warning) {
        var row = document.createElement('div');
        row.className = 'ui-notice ui-notice--warning';
        row.innerHTML = RS.kit.icon('alert');
        row.appendChild(_rsText('div', 'ui-notice-body', warning));
        wrap.appendChild(row);
    });
    return wrap;
}

// Resolves { confirmed, trust }. Deceptive links never offer the trust opt-out.
function rsLinkConfirm(href, opts) {
    opts = opts || {};
    var target = rsLinkTarget(href, opts.linkText);
    if (!target) return Promise.resolve({ confirmed: false, trust: false });
    var warned = target.warnings.length > 0;
    return new Promise(function(resolve) {
        _rsShowDialog({
            title: 'Leaving Ratspeak',
            message: 'This link opens in your browser:',
            detail: _rsLinkDetail(target),
            confirmText: warned ? 'Open anyway' : 'Open link',
            cancelText: 'Cancel',
            initialFocus: warned ? 'cancel' : 'confirm',
            hasInput: false,
            checkbox: warned || opts.allowTrust === false ? null : {
                label: "Don't ask again for " + target.trustKey,
            },
        }, function(result) {
            if (result && typeof result === 'object') {
                resolve({ confirmed: !!result.confirmed, trust: !!result.checked, host: target.trustKey });
            } else {
                resolve({ confirmed: !!result, trust: false, host: target.trustKey });
            }
        });
    });
}

// Resolves the chosen value or null on cancel. Choices are kit menu items: a bare glyph, the label
// with an optional hint, a trailing check on the value in effect, and a danger tone.
function rsChoice(opts) {
    opts = opts || {};
    return new Promise(function(resolve) {
        var built = _rsBuildSheet({
            title: opts.title || 'Choose',
            titleIcon: opts.titleIcon || '',
            titleIconType: opts.titleIconType || '',
            showTitle: opts.showTitle !== false,
            ariaLabel: opts.ariaLabel || opts.title || 'Choose an action',
            size: opts.size,
            sheetClass: opts.sheetClass || ''
        }, resolve);

        built.overlay.addEventListener('click', function(e) {
            if (e.target === built.overlay) built.dismiss(null);
        });

        if (opts.message) built.body.appendChild(_rsText('p', '', opts.message));

        if (opts.summary) {
            var summary = document.createElement('div');
            summary.className = 'ui-well rs-dialog-summary';
            if (opts.summary.primary) summary.appendChild(_rsText('b', '', opts.summary.primary));
            if (opts.summary.secondary) summary.appendChild(_rsText('span', '', opts.summary.secondary));
            if (opts.summary.note) summary.appendChild(_rsText('span', 'ui-help', opts.summary.note));
            built.body.appendChild(summary);
        }

        var choicesWrap = document.createElement('div');
        choicesWrap.className = 'ui-menu ui-menu--sheet rs-dialog-choices';
        choicesWrap.setAttribute('role', 'menu');
        choicesWrap.setAttribute('aria-label', opts.title || 'Choices');
        (opts.choices || []).forEach(function(choice) {
            var btn = document.createElement('button');
            btn.type = 'button';
            btn.className = 'ui-menu-item' + (choice.danger ? ' ui-menu-item--danger' : '');
            btn.setAttribute('role', choice.selected ? 'menuitemradio' : 'menuitem');
            btn.disabled = !!choice.disabled;
            // The value in effect now: checked, and where focus starts.
            if (choice.selected) {
                btn.classList.add('is-selected');
                btn.setAttribute('aria-checked', 'true');
            }
            if (choice.recommended) btn.classList.add('is-recommended');

            if (choice.icon) {
                var holder = document.createElement('span');
                holder.innerHTML = choice.icon;
                var glyph = holder.querySelector('svg');
                if (glyph) {
                    glyph.removeAttribute('width');
                    glyph.removeAttribute('height');
                    glyph.classList.add('ui-ico');
                    glyph.setAttribute('aria-hidden', 'true');
                    btn.appendChild(glyph);
                }
            }

            var text = document.createElement('span');
            text.className = 'lbl';
            var labelLine = document.createElement('span');
            labelLine.className = 'rs-dialog-choice-line';
            labelLine.appendChild(_rsText('span', '', choice.label));
            if (choice.recommended) labelLine.appendChild(_rsText('span', 'ui-tag ui-tag--accent', 'Recommended'));
            text.appendChild(labelLine);
            if (choice.hint) text.appendChild(_rsText('small', '', choice.hint));
            btn.appendChild(text);
            if (choice.meta) btn.appendChild(_rsText('span', 'trail', choice.meta));
            if (choice.selected) btn.insertAdjacentHTML('beforeend', RS.kit.icon('check', 'check'));
            btn.addEventListener('click', function() { built.dismiss(choice.value); });
            choicesWrap.appendChild(btn);
        });
        built.body.appendChild(choicesWrap);

        if (opts.cancelText !== false) {
            var cancelBtn = _rsButton(opts.cancelText || 'Cancel', 'cancel');
            cancelBtn.addEventListener('click', function() { built.dismiss(null); });
            built.footer.appendChild(cancelBtn);
        }

        built.sheet.addEventListener('keydown', function(e) {
            if (e.key === 'Escape') { e.stopPropagation(); built.dismiss(null); }
            if (e.key === 'Tab') _rsTrapTab(e, built.sheet, 'button');
        });

        _rsAttachDrag(built, function() { built.dismiss(null); });

        built.present();

        var firstBtn = choicesWrap.querySelector('.is-selected:not(:disabled)') ||
            choicesWrap.querySelector('.is-recommended:not(:disabled)') ||
            choicesWrap.querySelector('button:not(:disabled)');
        if (firstBtn) firstBtn.focus();
    });
}


// Returns a controller { update, success, error, close, isOpen, onClose }.
function rsProgress(opts) {
    opts = opts || {};
    var resolved = false;
    var _onClose = null;

    var built = _rsBuildSheet({
        title: opts.title || 'Working…',
        nativeBackDismissible: false
    }, function() {
        resolved = true;
        if (_onClose) _onClose();
    });
    built.sheet.setAttribute('aria-live', 'polite');
    // Not user-dismissible — overlay-tap intentionally not wired.

    var bodyInner = document.createElement('div');
    bodyInner.className = 'rs-progress';

    var mark = document.createElement('span');
    mark.className = 'ui-spin';
    mark.setAttribute('aria-hidden', 'true');

    var statusText = _rsText('span', '', opts.message || '');

    bodyInner.appendChild(mark);
    bodyInner.appendChild(statusText);
    built.body.appendChild(bodyInner);

    built.footer.hidden = true;

    var _timeoutId = null;

    if (typeof opts.onCancel === 'function') {
        var cancelBtn = _rsButton(opts.cancelText || 'Cancel', 'quiet');
        cancelBtn.addEventListener('click', function() {
            try { opts.onCancel(); } catch (_) {}
            close();
        });
        built.footer.hidden = false;
        built.footer.appendChild(cancelBtn);
    }

    function close() {
        if (resolved) return;
        if (_timeoutId) clearTimeout(_timeoutId);
        built.dismiss(null);
    }

    function finish(icon, tone, text) {
        var glyph = document.createElement('span');
        glyph.className = 'rs-progress-mark rs-progress-mark--' + tone;
        glyph.setAttribute('aria-hidden', 'true');
        glyph.innerHTML = RS.kit.icon(icon);
        bodyInner.replaceChild(glyph, bodyInner.firstChild);
        statusText.textContent = text;
    }

    var _errored = false;
    function showError(text) {
        if (_errored || resolved) return;
        _errored = true;
        if (_timeoutId) { clearTimeout(_timeoutId); _timeoutId = null; }
        finish('x-circle', 'error', text);
        // Replace any prior footer buttons so only Close remains.
        built.footer.innerHTML = '';
        var closeBtn = _rsButton('Close', 'primary');
        closeBtn.addEventListener('click', close);
        built.footer.hidden = false;
        built.footer.appendChild(closeBtn);
        closeBtn.focus();
    }

    if (opts.timeout && opts.timeout > 0) {
        _timeoutId = setTimeout(function() {
            if (!resolved) {
                showError(opts.timeoutMessage || 'This took too long. Try again.');
            }
        }, opts.timeout);
    }

    built.present();

    return {
        update: function(text) { statusText.textContent = text; },
        success: function(text) {
            finish('check-circle', 'success', text);
            setTimeout(close, 1500);
        },
        error: showError,
        close: close,
        isOpen: function() { return !resolved; },
        onClose: function(fn) { _onClose = fn; }
    };
}
