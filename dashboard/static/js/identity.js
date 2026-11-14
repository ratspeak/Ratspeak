var identityList = [];
var selectedIdentityHash = null;
var activeIdentityHash = null;
var RECOVERY_PHRASE_WORDS = 12;

function shareAddress(address, displayName) {
    if (!navigator.share) {
        RS.copyText(address).then(function(ok) {
            if (ok) showCopyConfirmationToast('Address');
        });
        return;
    }
    var title = displayName ? displayName + ' (Ratspeak)' : 'Ratspeak Address';
    navigator.share({
        title: title,
        text: address
    }).catch(function() {});
}

function activeIdentity() {
    for (var i = 0; i < identityList.length; i++) {
        if (identityList[i].is_active) return identityList[i];
    }
    return null;
}

function identityDisplayName(ident) {
    if (!ident) return 'Unnamed';
    return ident.display_name || ident.nickname || 'Unnamed';
}

function sortedIdentities() {
    return identityList.slice().sort(function(a, b) {
        return (a.created_at || 0) - (b.created_at || 0);
    });
}

function identityByHash(hash) {
    for (var i = 0; i < identityList.length; i++) {
        if (identityList[i].hash === hash) return identityList[i];
    }
    return null;
}

function selectedIdentity() {
    return identityByHash(selectedIdentityHash) || activeIdentity();
}

function originalIdentityHash() {
    var sorted = sortedIdentities();
    return sorted.length > 0 ? sorted[0].hash : null;
}

function isOriginalIdentity(hash) {
    return !!hash && hash === originalIdentityHash();
}



function identitySetInlineError(id, message) {
    var errEl = document.getElementById(id);
    if (!errEl) return;
    if (message) {
        errEl.textContent = message;
        errEl.style.display = '';
    } else {
        errEl.textContent = '';
        errEl.style.display = 'none';
    }
}

function identityPinError(err, fallback) {
    var msg = (err && err.message) ? err.message : (fallback || 'PIN failed.');
    return String(msg)
        .replace(/passcode/g, 'PIN')
        .replace(/Passcode/g, 'PIN');
}

// A key, phrase or access file is released with its identity's PIN, or after
// an explicit confirmation when it has none; the backend requires the same.
// `send(auth)` makes the request with `{ passcode }` or `{ confirmed: true }`
// and resolves its result. Resolves null when declined. In the PIN sheet every
// failure stays inline until the user cancels, which never resolves.
function authorizeIdentityKey(target, opts, send) {
    if (!(target && target.passcode_protected)) {
        return rsConfirm({
            title: opts.title,
            message: opts.message,
            confirmText: opts.confirmText,
            danger: !!opts.danger
        }).then(function(ok) { return ok ? send({ confirmed: true }) : null; });
    }
    return new Promise(function(resolve) {
        showIdentityModal(opts.title,
            '<p>' + escapeHtml(opts.message) + '</p>' +
            identityField('identity-key-pin', opts.pinLabel || 'PIN', { type: 'password', maxlength: 128, help: opts.pinHelp ? escapeHtml(opts.pinHelp) : '' }) +
            identityErrorHtml('identity-key-error'),
            function() {
                var pin = document.getElementById('identity-key-pin').value;
                var errEl = document.getElementById('identity-key-error');
                function fail(text) { if (errEl) { errEl.textContent = text; errEl.style.display = ''; } }
                if (!pin) { fail('Enter your PIN.'); return; }
                return send({ passcode: pin }).then(function(result) {
                    closeIdentityModal();
                    resolve(result);
                }, function(err) {
                    fail(identityPinError(err, 'PIN failed.'));
                });
            }, opts.danger ? 'danger' : '');
        var btn = document.getElementById('identity-modal-confirm');
        if (btn) { btn.textContent = opts.confirmText; btn.dataset.baseLabel = opts.confirmText; }
    });
}
window.authorizeIdentityKey = authorizeIdentityKey;

function identityPasscodeOptionHtml(prefix, opts) {
    opts = opts || {};
    var label = opts.label || 'Protect with a PIN';
    var help = opts.help || 'Ratspeak asks for it when it opens. A forgotten PIN can’t be recovered; your recovery phrase still works.';
    return '' +
        '<div class="ui-group"><label class="ui-prop" for="' + prefix + '-passcode-enable"><span class="ui-prop-text">' +
            '<span class="ui-prop-label">' + escapeHtml(label) + '</span><span class="ui-prop-desc">' + escapeHtml(help) + '</span></span>' +
            '<span class="ui-switch"><input type="checkbox" role="switch" id="' + prefix + '-passcode-enable"><span class="track"></span></span></label></div>' +
        '<div class="identity-fields" id="' + prefix + '-passcode-fields" hidden>' +
            identityField(prefix + '-passcode-new', 'PIN', { type: 'password', maxlength: 128, help: 'At least 6 characters.' }) +
            identityField(prefix + '-passcode-confirm', 'Confirm PIN', { type: 'password', maxlength: 128 }) +
        '</div>' +
        identityErrorHtml(prefix + '-passcode-error');
}

function bindIdentityPasscodeOption(prefix) {
    var enabled = document.getElementById(prefix + '-passcode-enable');
    var fields = document.getElementById(prefix + '-passcode-fields');
    if (!enabled || !fields) return;
    var sync = function() {
        fields.hidden = !enabled.checked;
        if (!enabled.checked) {
            var pw = document.getElementById(prefix + '-passcode-new');
            var confirm = document.getElementById(prefix + '-passcode-confirm');
            if (pw) pw.value = '';
            if (confirm) confirm.value = '';
            identitySetInlineError(prefix + '-passcode-error', null);
        }
    };
    enabled.addEventListener('change', sync);
    sync();
}

function readIdentityPasscodeOption(prefix, errorId) {
    var enabled = document.getElementById(prefix + '-passcode-enable');
    if (!enabled || !enabled.checked) {
        identitySetInlineError(errorId || (prefix + '-passcode-error'), null);
        return { enabled: false, passcode: '' };
    }
    var pwEl = document.getElementById(prefix + '-passcode-new');
    var confirmEl = document.getElementById(prefix + '-passcode-confirm');
    var passcode = pwEl ? pwEl.value : '';
    var confirm = confirmEl ? confirmEl.value : '';
    var errId = errorId || (prefix + '-passcode-error');
    if (passcode.length < 6) {
        identitySetInlineError(errId, 'PIN must be at least 6 characters.');
        return null;
    }
    if (passcode !== confirm) {
        identitySetInlineError(errId, "PINs don't match.");
        return null;
    }
    identitySetInlineError(errId, null);
    return { enabled: true, passcode: passcode };
}

function protectIdentityWithPasscode(hash, passcode) {
    if (!passcode) return Promise.resolve();
    if (!hash) return Promise.reject(new Error('Identity hash was not returned.'));
    return RS.invoke('set_identity_passcode', {
        args: { hash: hash, passcode: passcode }
    });
}

window.identityPasscodeOptionHtml = identityPasscodeOptionHtml;
window.bindIdentityPasscodeOption = bindIdentityPasscodeOption;
window.readIdentityPasscodeOption = readIdentityPasscodeOption;
window.protectIdentityWithPasscode = protectIdentityWithPasscode;

function bytesToBase64(bytes) {
    var binary = '';
    for (var i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i]);
    return btoa(binary);
}

function base64ToBytes(b64) {
    var raw = atob(b64);
    var arr = new Uint8Array(raw.length);
    for (var i = 0; i < raw.length; i++) arr[i] = raw.charCodeAt(i);
    return arr;
}

function hasAndroidIdentityExportBridge() {
    return typeof hasAndroidBridge === 'function' &&
        hasAndroidBridge() &&
        window.RatspeakAndroid &&
        typeof window.RatspeakAndroid.exportIdentityBackup === 'function';
}

function hasAndroidIdentityDocumentBridge() {
    return typeof hasAndroidBridge === 'function' &&
        hasAndroidBridge() &&
        window.RatspeakAndroid &&
        typeof window.RatspeakAndroid.saveIdentityDocument === 'function';
}

function hasAndroidIdentityImportBridge() {
    return typeof hasAndroidBridge === 'function' &&
        hasAndroidBridge() &&
        window.RatspeakAndroid &&
        typeof window.RatspeakAndroid.importIdentityBackup === 'function';
}

function resetPendingIdentityImport() {
    window._identityImportFromSetup = false;
    window._identityImportFormat = null;
    window._identityImportPasscode = null;
}

function identityImportFormatChoices() {
    return [
        { label: 'Ratspeak backup', value: 'ratspeak', hint: 'A .rsi file and the backup PIN you set.' },
        { label: 'Reticulum identity key', value: 'reticulum', hint: 'A key file, or the key as base32, base64, or hex text.' },
        { label: 'Recovery phrase', value: 'phrase', hint: 'Your ' + RECOVERY_PHRASE_WORDS + ' words. Restores the identity and its address.' }
    ];
}

function identityExportFormatChoices() {
    return [
        { label: 'Ratspeak backup', value: 'ratspeak', tag: 'Recommended', hint: 'A .rsi file locked with a backup PIN. Restores your name too.' },
        { label: 'Reticulum identity file', value: 'reticulum', hint: 'The raw private key, unencrypted. For other Reticulum apps.' },
        { label: 'Reticulum key as text', value: 'reticulum-base32', hint: 'The same key in base32, unencrypted.' }
    ];
}

// A pick-and-go list: choosing an option is the action. Resolves its value, or null.
function identityChoiceDialog(title, intro, choices) {
    return new Promise(function(resolve) {
        var picked = null;
        var built = RS.kit.dialog({
            title: title,
            size: 'md',
            body: (intro ? '<p>' + escapeHtml(intro) + '</p>' : '') +
                '<div class="ui-choices" role="menu">' + choices.map(function(c, i) {
                    return '<button type="button" class="ui-menu-item" role="menuitem" data-choice="' + i + '"><span class="lbl"><span>' + escapeHtml(c.label) +
                        (c.tag ? '<span class="ui-tag ui-tag--accent">' + escapeHtml(c.tag) + '</span>' : '') + '</span>' +
                        (c.hint ? '<small>' + escapeHtml(c.hint) + '</small>' : '') + '</span>' + RS.kit.icon('chevron-right', 'trail') + '</button>';
                }).join('') + '</div>',
            onClose: function() { resolve(picked); }
        });
        built.dialog.addEventListener('click', function(e) {
            var btn = e.target.closest('[data-choice]');
            if (!btn) return;
            picked = choices[Number(btn.getAttribute('data-choice'))].value;
            built.close(picked);
        });
    });
}

function chooseIdentityImportFormat() {
    return identityChoiceDialog('Import or restore', '', identityImportFormatChoices());
}

function chooseIdentityExportFormat(name) {
    return identityChoiceDialog('Export identity', 'Choose a format for ' + (name || 'this identity') + '. Messages and settings stay on this device.', identityExportFormatChoices());
}

// `noun` names the PIN in its labels: 'PIN' (default) or 'backup PIN'.
function backupPinFieldsHtml(prefix, confirm, noun) {
    noun = noun || 'PIN';
    return identityField(prefix + '-pin', noun.charAt(0).toUpperCase() + noun.slice(1), { type: 'password', maxlength: 128, help: 'At least 6 characters.' }) +
        (confirm ? identityField(prefix + '-pin-confirm', 'Confirm ' + noun, { type: 'password', maxlength: 128 }) : '') +
        identityErrorHtml(prefix + '-pin-error');
}

function readBackupPin(prefix, confirm) {
    var pinEl = document.getElementById(prefix + '-pin');
    var pin = pinEl ? pinEl.value : '';
    var errEl = document.getElementById(prefix + '-pin-error');
    if (pin.length < 6) {
        if (errEl) { errEl.textContent = 'PIN must be at least 6 characters.'; errEl.style.display = ''; }
        return null;
    }
    if (confirm) {
        var confirmEl = document.getElementById(prefix + '-pin-confirm');
        if (pin !== (confirmEl ? confirmEl.value : '')) {
            if (errEl) { errEl.textContent = "PINs don't match."; errEl.style.display = ''; }
            return null;
        }
    }
    if (errEl) { errEl.textContent = ''; errEl.style.display = 'none'; }
    return pin;
}

function androidIdentityExportError(message) {
    var err = new Error(message || 'Identity export failed');
    err.cancelled = message === 'Export cancelled';
    return err;
}

function androidIdentityImportError(message) {
    var err = new Error(message || 'Identity import failed');
    err.cancelled = message === 'Import cancelled';
    return err;
}

function openIdentityBackupWithAndroid() {
    if (window._androidIdentityImportPending) {
        return Promise.reject(new Error('Identity import already in progress'));
    }
    return new Promise(function(resolve, reject) {
        window._androidIdentityImportPending = true;
        window._onAndroidIdentityImportResult = function(result) {
            window._androidIdentityImportPending = false;
            window._onAndroidIdentityImportResult = null;
            if (result && result.success) {
                resolve({
                    fileName: result.file_name || 'identity file',
                    fileSize: result.file_size || 0,
                    backupBase64: result.backup_base64 || ''
                });
            } else {
                reject(androidIdentityImportError((result && result.error) || 'Identity import failed'));
            }
        };
        try {
            window.RatspeakAndroid.importIdentityBackup();
        } catch (err) {
            window._androidIdentityImportPending = false;
            window._onAndroidIdentityImportResult = null;
            reject(err);
        }
    });
}

function saveIdentityDocumentWithAndroid(fileName, dataBase64, mimeType) {
    if (window._androidIdentityExportPending) {
        return Promise.reject(new Error('Identity export already in progress'));
    }
    return new Promise(function(resolve, reject) {
        window._androidIdentityExportPending = true;
        window._onAndroidIdentityExportResult = function(result) {
            window._androidIdentityExportPending = false;
            window._onAndroidIdentityExportResult = null;
            if (result && result.success) {
                resolve({ method: 'android-document', uri: result.uri || '', fileName: fileName });
            } else {
                reject(androidIdentityExportError((result && result.error) || 'Export failed'));
            }
        };
        try {
            window.RatspeakAndroid.saveIdentityDocument(fileName, dataBase64, mimeType || 'application/octet-stream');
        } catch (err) {
            window._androidIdentityExportPending = false;
            window._onAndroidIdentityExportResult = null;
            reject(err);
        }
    });
}

function saveIdentityBackupWithAndroid(fileName, backupBase64) {
    if (window._androidIdentityExportPending) {
        return Promise.reject(new Error('Identity export already in progress'));
    }
    return new Promise(function(resolve, reject) {
        window._androidIdentityExportPending = true;
        window._onAndroidIdentityExportResult = function(result) {
            window._androidIdentityExportPending = false;
            window._onAndroidIdentityExportResult = null;
            if (result && result.success) {
                resolve({ method: 'android-document', uri: result.uri || '', fileName: fileName });
            } else {
                reject(androidIdentityExportError((result && result.error) || 'Export failed'));
            }
        };
        try {
            window.RatspeakAndroid.exportIdentityBackup(fileName, backupBase64);
        } catch (err) {
            window._androidIdentityExportPending = false;
            window._onAndroidIdentityExportResult = null;
            reject(err);
        }
    });
}

// How the desktop download of `url` that `start` begins ended, as the save
// dialog and the WebView report it: `saved`, `cancelled` or `failed`. Null
// elsewhere, or when no report comes within the dialog's ten minutes.
function desktopDownloadResult(url, start) {
    if (typeof isTauriDesktop !== 'function' || !isTauriDesktop() ||
            !window.RS || typeof RS.listen !== 'function') {
        start();
        return Promise.resolve(null);
    }
    var report = null;
    var ended = new Promise(function(resolve) { report = resolve; });
    return Promise.resolve(RS.listen('download_result', function(data) {
        if (data && data.url === url) report(data.status || null);
    })).catch(function() { return null; }).then(function(unlisten) {
        if (typeof unlisten !== 'function') {
            start();
            return null;
        }
        var timer = setTimeout(function() { report(null); }, 600000);
        start();
        return ended.then(function(status) {
            clearTimeout(timer);
            unlisten();
            return status;
        });
    });
}

function saveBytesToUserFile(bytes, fileName, mimeType, backupBase64) {
    if (hasAndroidIdentityDocumentBridge()) {
        return saveIdentityDocumentWithAndroid(fileName, backupBase64 || bytesToBase64(bytes), mimeType);
    }
    if (hasAndroidIdentityExportBridge() && /\.rsi$/i.test(fileName || '')) {
        return saveIdentityBackupWithAndroid(fileName, backupBase64 || bytesToBase64(bytes));
    }

    var blob = new Blob([bytes], { type: mimeType || 'application/octet-stream' });
    var url = URL.createObjectURL(blob);
    return desktopDownloadResult(url, function() {
        var a = document.createElement('a');
        a.href = url;
        a.download = fileName;
        a.style.display = 'none';
        document.body.appendChild(a);
        a.click();
        a.remove();
    }).then(function(status) {
        var revoke = function() { try { URL.revokeObjectURL(url); } catch (_) {} };
        if (status) revoke(); else setTimeout(revoke, 60000);
        return { method: 'download', fileName: fileName, status: status };
    });
}

function renderNetworkIdentityCard() {
    var container = document.getElementById('net-identity-card');
    if (!container) return;

    var active = null;
    for (var i = 0; i < identityList.length; i++) {
        if (identityList[i].is_active) { active = identityList[i]; break; }
    }

    if (!active) {
        container.innerHTML = '<div class="text-muted-color text-sm">No active identity.</div>';
        return;
    }

    var nickname = escapeHtml(active.display_name || active.nickname || 'Unnamed');
    var lxmfHash = active.lxmf_hash || '';
    var avatarHtml = identityAvatar(lxmfHash, 40);

    container.innerHTML =
        '<div class="identity-summary-inline">' +
            '<div class="identity-summary-avatar">' + avatarHtml + '</div>' +
            '<div class="identity-summary-meta">' +
                '<div class="identity-summary-name">' + nickname + '</div>' +
                '<div class="font-mono inline-hint-sm identity-summary-hash">' + lxmfHash + '</div>' +
            '</div>' +
        '</div>';
}

// One page for identities: the list, and the chosen identity's profile,
// security and backup, and addresses. On a phone the list is its own screen.
var _identitySwitchingHash = null;
var _identitySwitchFailed = null;
var _identityPhoneDetailOpen = false;
var _identityAutoLockSeconds = null;

function identityField(id, label, opts) {
    opts = opts || {};
    return '<div class="ui-field"' + (opts.fieldId ? ' id="' + opts.fieldId + '"' : '') + (opts.hidden ? ' hidden' : '') + '>' +
        '<label class="ui-label" for="' + id + '">' + escapeHtml(label) + '</label>' +
        '<input class="ui-input' + (opts.mono ? ' ui-input--mono' : '') + '" id="' + id + '" type="' + (opts.type || 'text') + '"' +
            (opts.maxlength ? ' maxlength="' + opts.maxlength + '"' : '') +
            (opts.inputmode ? ' inputmode="' + opts.inputmode + '"' : '') +
            (opts.placeholder ? ' placeholder="' + escapeHtml(opts.placeholder) + '"' : '') +
            (opts.value ? ' value="' + escapeHtml(opts.value) + '"' : '') +
            ' autocomplete="off" autocorrect="off" autocapitalize="none" spellcheck="false"' +
            (opts.help ? ' aria-describedby="' + id + '-help"' : '') + '>' +
        (opts.help ? '<p class="ui-help" id="' + id + '-help">' + opts.help + '</p>' : '') +
    '</div>';
}

// The inline error under a dialog's fields; identitySetInlineError fills it.
function identityErrorHtml(id) {
    return '<p class="ui-error" id="' + id + '" style="display:none"></p>';
}

function identityWhereText(ident) {
    if (ident.is_active) return 'In use on this device';
    if (ident.is_hardware) return 'On a YubiKey, not in use';
    return 'Stored on this device, not in use';
}

function identityCreatedText(ident) {
    if (!ident.created_at) return '';
    var ms = ident.created_at < 1e11 ? ident.created_at * 1000 : ident.created_at;
    return new Date(ms).toLocaleDateString('en-US', { month: 'short', day: 'numeric', year: 'numeric' });
}

function _identityProp(label, desc, control, id) {
    return '<div class="ui-prop"' + (id ? ' id="' + id + '"' : '') + '><span class="ui-prop-text"><span class="ui-prop-label">' + label + '</span>' +
        '<span class="ui-prop-desc">' + desc + '</span></span>' + (control || '') + '</div>';
}

function _identitySmallBtn(label, action, quiet) {
    return '<button type="button" class="ui-btn ui-btn--sm' + (quiet ? ' ui-btn--quiet' : '') + '" data-identity-action="' + action + '">' + label + '</button>';
}

function _identityAutoLockLabel(seconds) {
    if (!seconds || seconds <= 0) return 'Off';
    if (seconds % 3600 === 0) { var h = seconds / 3600; return h + (h === 1 ? ' hour' : ' hours'); }
    if (seconds % 60 === 0) { var m = seconds / 60; return m + (m === 1 ? ' minute' : ' minutes'); }
    return seconds + ' seconds';
}

function identityRowHtml(ident, current) {
    var tag = ident.is_active ? '<span class="ui-tag ui-tag--accent">In use</span>' : '';
    var address = ident.lxmf_hash || ident.hash || '';
    return '<li><button type="button" class="ui-row" data-identity="' + escapeHtml(ident.hash) + '"' + (current ? ' aria-current="true"' : '') + '>' +
        RS.kit.avatar(address, 'row') +
        '<span class="ui-row-main"><span class="ui-row-top"><span class="ui-row-title">' + escapeHtml(identityDisplayName(ident)) + '</span>' + tag + '</span>' +
        '<span class="ui-row-sub"><span class="t">' + (ident.is_hardware ? 'Hardware key · ' : '') +
            '<span class="ui-hash" dir="ltr">' + escapeHtml(RS.kit.hash.short(address)) + '</span></span></span></span></button></li>';
}

function identityDetailHtml(ident) {
    var name = identityDisplayName(ident);
    var original = isOriginalIdentity(ident.hash);
    var switching = _identitySwitchingHash === ident.hash;
    var actions;
    if (ident.is_active) {
        actions = '<div class="person-actions"><button type="button" class="ui-btn ui-btn--primary ui-btn--pair" data-identity-action="share">' + RS.kit.icon('qr') + 'Share contact card</button></div>';
    } else {
        actions = '<div class="person-actions">' +
            (switching
                ? '<button type="button" class="ui-btn ui-btn--primary ui-btn--pair" aria-busy="true" disabled><span class="ui-spin" aria-hidden="true"></span>Switching…</button>'
                : '<button type="button" class="ui-btn ui-btn--primary ui-btn--pair" data-identity-action="switch">Switch to this identity</button>') +
            '<button type="button" class="ui-btn ui-btn--pair" data-identity-action="share">' + RS.kit.icon('qr') + 'Share card</button></div>';
    }
    var head = '<header class="person-head"><div class="person-wrap">' +
        RS.kit.avatar(ident.lxmf_hash || ident.hash, 64) +
        '<div class="person-id"><div class="person-name-row"><h2 class="ui-detail-title" id="identity-detail-name">' + escapeHtml(name) + '</h2>' +
            (ident.is_active ? '<button type="button" class="ui-icon-btn ui-icon-btn--sm" aria-label="Rename" title="Rename" data-identity-action="rename">' + RS.kit.icon('edit') + '</button>' : '') + '</div>' +
            '<p class="person-status">' + escapeHtml(identityWhereText(ident)) + '</p></div>' +
        actions + '</div></header>';

    var failed = _identitySwitchFailed && _identitySwitchFailed.hash === ident.hash
        ? '<div class="ui-notice ui-notice--error" role="alert">' + RS.kit.icon('alert') + '<div class="ui-notice-body"><b>Couldn’t switch to ' + escapeHtml(name) + '.</b> Ratspeak kept ' +
            escapeHtml(_identitySwitchFailed.kept || 'your identity') + ' in use. Check its PIN and try again.</div>' +
            '<button type="button" class="ui-btn ui-btn--sm" data-identity-action="switch">Try again</button></div>'
        : '';

    var profile = '';
    if (ident.is_active) {
        var status = typeof resolveActiveProfileStatus === 'function' ? String(resolveActiveProfileStatus() || '').trim() : '';
        profile = '<section aria-labelledby="identity-g-profile"><h3 class="ui-section-title" id="identity-g-profile">Profile</h3><div class="ui-group">' +
            _identityProp('Status', status ? '“' + escapeHtml(status) + '”' : 'Not set. Ratspeak users see it under your name.', _identitySmallBtn(status ? 'Edit' : 'Set status', 'status')) +
            '</div></section>';
    }

    var protectedIdentity = ident.is_hardware || ident.passcode_protected;
    var autoLock = protectedIdentity
        ? _identityProp('Auto-lock', 'Locks again this long after you unlock it.',
            '<button type="button" class="ui-select ui-select--inline" data-identity-action="autolock" aria-haspopup="menu"><span class="v">' +
                escapeHtml(_identityAutoLockLabel(_identityAutoLockSeconds)) + '</span>' + RS.kit.icon('chevron-down') + '</button>')
        : '';
    var security;
    if (ident.is_hardware) {
        var canChangePin = typeof isMobile !== 'function' || !isMobile();
        security = '<section aria-labelledby="identity-g-sec"><h3 class="ui-section-title" id="identity-g-sec">Security and backup</h3><div class="ui-group">' +
            _identityProp('Hardware key', 'YubiKey' + (ident.hw_serial ? ' · serial ' + escapeHtml(String(ident.hw_serial)) : ''), canChangePin ? _identitySmallBtn('Change PIN', 'hardware-pin') : '') +
            autoLock +
            '</div><p class="ui-group-foot">The private key never leaves the key, so there’s no recovery phrase or export. Keep the key safe.</p></section>';
    } else {
        security = '<section aria-labelledby="identity-g-sec"><h3 class="ui-section-title" id="identity-g-sec">Security and backup</h3><div class="ui-group">' +
            (ident.passcode_protected
                ? _identityProp('PIN', 'On. Ratspeak asks for it when it opens.', _identitySmallBtn('Change PIN', 'pin-change'))
                : _identityProp('PIN', 'Off. Ratspeak opens without asking.', _identitySmallBtn('Set PIN', 'pin-set'))) +
            (ident.has_mnemonic
                ? _identityProp('Recovery phrase', RECOVERY_PHRASE_WORDS + ' words that restore this identity on any device.', _identitySmallBtn('View', 'phrase'))
                : _identityProp('Recovery phrase', 'This identity has no recovery phrase on this device. Export a backup instead.', '')) +
            _identityProp('Export', 'Save a backup file or a Reticulum key.', _identitySmallBtn('Export…', 'export')) +
            autoLock +
            '</div></section>';
    }

    var lxmf = ident.lxmf_hash || '';
    var addresses = '<section aria-labelledby="identity-g-addr"><h3 class="ui-section-title" id="identity-g-addr">Addresses</h3>' +
        personDlHtml([
            ['LXMF address', RS.kit.hash.copy(lxmf, { form: 'full', noun: 'LXMF address' }) + '<span class="ui-dl-hint">Share this so people can message you.</span>'],
            ['Identity hash', RS.kit.hash.copy(ident.hash, { form: 'full', noun: 'Identity hash' })]
        ], true) + '</section>';
    var created = identityCreatedText(ident);
    var detailRows = [];
    if (created) detailRows.push(['Created', escapeHtml(created)]);
    detailRows.push(['Key', ident.is_hardware ? 'Hardware key' : 'Kept on this device' + (ident.passcode_protected ? ', locked with a PIN' : '')]);
    var details = '<section aria-labelledby="identity-g-det"><h3 class="ui-section-title" id="identity-g-det">Details</h3>' + personDlHtml(detailRows, true) + '</section>';
    var deleteLabel = ident.is_hardware ? 'Remove identity' : 'Delete identity';
    var danger = '<div class="person-danger">' +
        (original
            ? '<button type="button" class="ui-menu-item ui-menu-item--danger" disabled aria-describedby="identity-delete-why">' + RS.kit.icon('trash') + '<span class="lbl">' + deleteLabel + '</span></button>' +
                '<p class="ui-group-foot" id="identity-delete-why">Your first identity can’t be deleted.</p>'
            : '<button type="button" class="ui-menu-item ui-menu-item--danger" data-identity-action="delete">' + RS.kit.icon('trash') + '<span class="lbl">' + deleteLabel + '</span></button>') +
        '</div>';

    return head + '<div class="person-wrap person-body"><div class="person-main">' + failed + profile + security + '</div>' +
        '<aside class="person-side" aria-label="Addresses and identity actions">' + addresses + details + danger + '</aside></div>';
}

function renderIdentityPage() {
    var list = document.getElementById('identity-list');
    var detail = document.getElementById('identity-detail');
    var pane = document.getElementById('identity-pane');
    var count = document.getElementById('identity-count');
    if (!list || !detail) return;
    var sorted = sortedIdentities();
    if (count) count.innerHTML = sorted.length ? '<span class="ui-sr">, </span>' + sorted.length : '';
    var phone = isCompactLayout();
    var selected = selectedIdentity();
    if (selected) selectedIdentityHash = selected.hash;

    var listHtml = sorted.length
        ? '<div class="ui-list"><ul class="person-rows">' + sorted.map(function(ident) {
            return identityRowHtml(ident, !phone && selected && ident.hash === selected.hash);
        }).join('') + '</ul></div>' +
            (sorted.length === 1 ? '<p class="ui-list-note">Keep separate identities for work, family, or testing. Each one has its own address, contacts, and messages.</p>' : '')
        : '<div class="ui-empty ui-empty--quiet"><p class="ui-empty-body">No identity is loaded.</p></div>';
    if (list._rsLastHtml !== listHtml) {
        list._rsLastHtml = listHtml;
        list.innerHTML = listHtml;
    }

    var open = phone && _identityPhoneDetailOpen && !!selected;
    if (pane) pane.classList.toggle('is-covered', open);
    detail.classList.toggle('is-open', open);
    var html = selected
        ? (phone ? '<header class="ui-pane-head has-back"><button type="button" class="ui-icon-btn" data-identity-action="back" aria-label="Back to identities" title="Back">' + RS.kit.icon('chevron-left') + '</button></header>' : '') +
            identityDetailHtml(selected)
        : '';
    if (detail._rsLastHtml !== html) {
        detail._rsLastHtml = html;
        detail.innerHTML = html;
    }
}

function selectIdentity(hash) {
    selectedIdentityHash = hash;
    if (isCompactLayout()) _identityPhoneDetailOpen = true;
    renderIdentityPage();
    var detail = document.getElementById('identity-detail');
    if (detail) detail.scrollTop = 0;
}

// Settings' Identity row and the sidebar open this; it shows the identity in use.
function openIdentityPage(hash) {
    selectedIdentityHash = hash || activeIdentityHash || selectedIdentityHash;
    _identityPhoneDetailOpen = !!hash;
    if (typeof switchView === 'function') switchView('identity');
    renderIdentityPage();
}
window.openIdentityPage = openIdentityPage;

function openIdentityAddMenu(trigger) {
    var items = [
        { label: 'Create identity', hint: 'A new address with its own contacts and messages.', icon: 'plus', onSelect: createNewIdentity },
        { label: 'Import or restore', hint: 'From a backup file, a key, or a recovery phrase.', icon: 'download', onSelect: function() {
            window._identityImportFromSetup = false;
            importIdentity();
        } }
    ];
    var hideHardware = typeof supportsHardwareIdentities === 'function' && !supportsHardwareIdentities();
    if (!hideHardware) {
        items.push({ label: 'Add hardware key', hint: 'Keep the private key on a YubiKey.', icon: 'key', onSelect: function() { openHardwareWizard({ fromSetup: false }); } });
    }
    RS.kit.menu(trigger, { title: 'Add identity', items: items });
}

function openIdentityAutoLockMenu(trigger) {
    var options = [0, 300, 900, 1800, 3600];
    RS.kit.menu(trigger, {
        title: 'Auto-lock',
        caption: 'Lock after',
        items: options.map(function(seconds) {
            return {
                label: _identityAutoLockLabel(seconds),
                checked: (_identityAutoLockSeconds || 0) === seconds,
                onSelect: function() {
                    var previous = _identityAutoLockSeconds;
                    _identityAutoLockSeconds = seconds;
                    renderIdentityPage();
                    RS.invoke('set_hardware_lock_timeout', { seconds: seconds }).catch(function(err) {
                        _identityAutoLockSeconds = previous;
                        renderIdentityPage();
                        showToast((err && err.message) || 'Could not update auto-lock', 'toast-error', 5000);
                    });
                }
            };
        })
    });
}

function _identityRunAction(action, trigger) {
    var ident = selectedIdentity();
    if (action === 'back') {
        _identityPhoneDetailOpen = false;
        renderIdentityPage();
        return;
    }
    if (!ident) return;
    if (action === 'switch') switchToIdentity(ident.hash);
    else if (action === 'share') {
        if (typeof openIdentityShareScreen === 'function') openIdentityShareScreen(ident.hash);
        else shareAddress(ident.lxmf_hash || ident.hash, identityDisplayName(ident));
    } else if (action === 'rename') openRenameIdentityDialog(ident);
    else if (action === 'status') { if (typeof openIdentityStatusEditor === 'function') openIdentityStatusEditor(); }
    else if (action === 'pin-set') openSetPasscodeModal(ident, false);
    else if (action === 'pin-change') openSetPasscodeModal(ident, true);
    else if (action === 'pin-remove') openRemovePasscodeModal(ident);
    else if (action === 'hardware-pin') openHardwareChangePinModal(ident);
    else if (action === 'phrase') viewRecoveryPhrase(ident);
    else if (action === 'export') exportIdentityBackup(ident.hash);
    else if (action === 'autolock') openIdentityAutoLockMenu(trigger);
    else if (action === 'delete') deleteIdentityByHash(ident.hash);
}

// Rename the identity in use: Ratspeak announces it and retires the old name everywhere it was shared.
function openRenameIdentityDialog(ident) {
    var oldName = ident.display_name || ident.nickname || '';
    var built = RS.kit.dialog({
        title: 'Rename identity',
        body: identityField('identity-rename-input', 'Name', { value: oldName, maxlength: 32, help: 'Up to 32 characters.' }) +
            identityErrorHtml('identity-rename-error') +
            (oldName ? '<div class="ui-notice ui-notice--info">' + RS.kit.icon('info') + '<div class="ui-notice-body">Ratspeak announces the new name and stops using “' + escapeHtml(oldName) +
                '” everywhere it was shared, including channel nicknames and saved hubs that still use it. Names you set for a single hub stay.</div></div>' : ''),
        foot: '<button type="button" class="ui-btn ui-btn--quiet ui-desk-only" data-ui-close>Cancel</button><button type="button" class="ui-btn ui-btn--primary" id="identity-rename-save">Rename</button>'
    });
    var input = built.q('#identity-rename-input');
    var save = built.q('#identity-rename-save');
    function submit() {
        var name = input.value.trim();
        if (!name) { identitySetInlineError('identity-rename-error', 'Enter a name.'); return; }
        if (name === oldName) { built.close(null); return; }
        save.disabled = true;
        RS.invoke('api_set_display_name', { args: { display_name: name, identity_id: ident.hash } }).then(function() {
            // Refresh the cached name so no consumer keeps offering the superseded one.
            try { localStorage.setItem('ratspeak_identity_name', name); } catch (_) {}
            // The backend retired the old name from saved hub bookmarks; reload them.
            if (typeof channelsRefreshSavedHubs === 'function') channelsRefreshSavedHubs();
            built.close(name);
            showToast('Name saved and announced', 'toast-success', 3000);
            loadIdentities();
        }).catch(function(err) {
            save.disabled = false;
            identitySetInlineError('identity-rename-error', (err && err.message) ? err.message : 'Could not save the name');
        });
    }
    save.addEventListener('click', submit);
    input.addEventListener('keydown', function(e) { if (e.key === 'Enter') { e.preventDefault(); submit(); } });
    if (!RS.kit.compact()) input.select();
}

function _hydrateIdentityAutoLock() {
    if (!window.RS || typeof RS.invoke !== 'function') return;
    RS.invoke('api_app_settings').then(function(data) {
        if (data && data.hardware_session_timeout !== undefined) {
            _identityAutoLockSeconds = parseInt(data.hardware_session_timeout, 10) || 0;
            renderIdentityPage();
        }
    }).catch(function() {});
}

document.addEventListener('DOMContentLoaded', function() {
    var view = document.getElementById('view-identity');
    var addBtn = document.getElementById('identity-add-btn');
    if (addBtn) addBtn.addEventListener('click', function() { openIdentityAddMenu(addBtn); });
    if (view) view.addEventListener('click', function(e) {
        var row = e.target.closest('.ui-row[data-identity]');
        if (row) { selectIdentity(row.getAttribute('data-identity')); return; }
        var action = e.target.closest('[data-identity-action]');
        if (action && !action.disabled) _identityRunAction(action.getAttribute('data-identity-action'), action);
    });
    if (typeof watchViewLayout === 'function') watchViewLayout('identity', renderIdentityPage);
    _hydrateIdentityAutoLock();
});

RS.listen('app_settings_updated', function(data) {
    if (!data || data.hardware_session_timeout === undefined) return;
    _identityAutoLockSeconds = parseInt(data.hardware_session_timeout, 10) || 0;
    renderIdentityPage();
});

function loadIdentities(retryCount) {
    retryCount = retryCount || 0;
    RS.invoke('api_list_identities').then(function(data) {
        identityList = data || [];
        var _activeIdent = null;
        for (var i = 0; i < identityList.length; i++) {
            if (identityList[i].is_active) {
                activeIdentityHash = identityList[i].hash;
                _activeIdent = identityList[i];
                break;
            }
        }
        // DB-backed update — survives a race with LXMF init on startup.
        if (_activeIdent && typeof updateHeaderIdentity === 'function') {
            updateHeaderIdentity(
                _activeIdent.lxmf_hash || _activeIdent.hash || '',
                _activeIdent.display_name || _activeIdent.nickname || '',
                typeof profileStatusFromPayload === 'function' ? profileStatusFromPayload(_activeIdent) : null
            );
        }
        document.body.classList.toggle('multi-identity', identityList.length > 1);
        // Per-section try/catch — one render failure shouldn't block others.
        try { renderIdentityPage(); }
        catch(e) { window.RS.diag('error', '[Identity] Page render error:', e); }

        try { renderNetworkIdentityCard(); }
        catch(e) {}

        try { if (typeof syncSettingsIdentityActions === 'function') syncSettingsIdentityActions(); }
        catch(e) {}

        try { if (typeof renderMsgProfileStrip === 'function') renderMsgProfileStrip(); }
        catch(e) {}
    }).catch(function(err) {
        window.RS.diag('error', '[Identity] Failed to load identities:', err);
        if (retryCount < 3) {
            setTimeout(function() { loadIdentities(retryCount + 1); }, 1000 * (retryCount + 1));
        }
    });
}




// Switches this window asked for; their own replies say why one failed.
var _switchRequestsPending = 0;

function switchToIdentity(hash) {
    // The detail shows the switch in its button; a failure becomes a notice there.
    _identitySwitchingHash = hash;
    _identitySwitchFailed = null;
    renderIdentityPage();
    // Backend reloads LXMF manager + emits identity_switched, which drives
    // full state cleanup + re-emits initial state for the new identity.
    _switchRequestsPending += 1;
    RS.invoke('switch_identity', { hash: hash }).then(function() {
        _switchRequestsPending -= 1;
        _identitySwitchingHash = null;
    }, function(err) {
        _switchRequestsPending -= 1;
        _identitySwitchingHash = null;
        _identityNoteSwitchFailure(hash, (err && err.message) ? err.message : 'Could not switch identity');
        loadIdentities();
    });
}

// On the Identity page the failure is a notice with Try again; elsewhere it is a toast.
function _identityNoteSwitchFailure(hash, message) {
    var kept = activeIdentity();
    if (typeof isViewActive === 'function' && isViewActive('identity') && hash) {
        _identitySwitchFailed = { hash: hash, kept: kept ? identityDisplayName(kept) : '' };
        renderIdentityPage();
        return;
    }
    showToast(message, 'toast-error', 4000);
}


function createNewIdentity() {
    showIdentityModal('Create an identity',
        '<p>A new identity has its own address, contacts, and messages. Switch between identities at any time.</p>' +
        identityField('identity-modal-nickname', 'Name', { maxlength: 32, help: 'People see this name when you message them. You can change it later.' }) +
        identityPasscodeOptionHtml('identity-create'),
        function() {
            var nickname = document.getElementById('identity-modal-nickname').value.trim();
            var passcodeOption = readIdentityPasscodeOption('identity-create');
            if (!passcodeOption) return;
            var passcode = passcodeOption.passcode;
            return RS.invoke('api_create_identity', { args: { nickname: nickname } }).then(function(data) {
                closeIdentityModal();
                var hash = data && (data.hash || data.identity_hash);
                var protect = passcode ? protectIdentityWithPasscode(hash, passcode).catch(function(err) {
                    var msg = identityPinError(err, 'Could not encrypt identity with PIN');
                    showToast('Identity created, but the PIN could not be set: ' + msg, 'toast-error', 7000);
                }) : Promise.resolve();
                var mnemonic = data && data.mnemonic;
                return protect.then(function() {
                    if (mnemonic) {
                        showRecoveryPhraseBackup(mnemonic, function() {
                            showToast('Identity created', 'toast-success', 3000);
                            loadIdentities();
                        }, { passcodeProtected: !!passcode, name: nickname });
                    } else {
                        showToast('Identity created', 'toast-success', 3000);
                        loadIdentities();
                    }
                });
            }).catch(function(err) {
                showToast(err && err.message ? err.message : 'Could not create the identity', 'toast-error', 3000);
                loadIdentities();
            });
        }, '', 'Create identity'
    );
    bindIdentityPasscodeOption('identity-create');
}

function recoveryPhraseWords(mnemonic) {
    return String(mnemonic || '').trim().split(/\s+/).filter(Boolean);
}

window.recoveryPhraseWords = recoveryPhraseWords;

// Recovery-phrase reveal as a standalone full-screen overlay. Software and
// hardware setup flows share the reveal/copy/check pattern so users learn
// one recovery ritual instead of separate backup flows.
// The twelve words in the kit's phrase component; covered until shown when asked.
function recoveryPhraseHtml(words, covered, id) {
    return '<div class="ui-phrase' + (covered ? ' is-covered' : '') + '"' + (id ? ' id="' + id + '"' : '') + '><ol' + (covered ? ' aria-hidden="true"' : '') + '>' +
        (words || []).map(function(w) { return '<li>' + escapeHtml(w) + '</li>'; }).join('') + '</ol>' +
        (covered ? '<button type="button" class="ui-phrase-cover">' + RS.kit.icon('lock') + 'Show the ' + RECOVERY_PHRASE_WORDS + ' words</button>' : '') + '</div>';
}
window.recoveryPhraseHtml = recoveryPhraseHtml;

// First backup (opts.requireConfirm, the default): the phrase in full and an
// acknowledgement. Viewing again (requireConfirm false): covered until shown.
// opts: { name, requireConfirm, button, title }
function showRecoveryPhraseBackup(mnemonic, onDone, opts) {
    opts = opts || {};
    var words = recoveryPhraseWords(mnemonic);
    if (words.length !== RECOVERY_PHRASE_WORDS) {
        if (typeof onDone === 'function') onDone();
        return;
    }
    var name = opts.name || 'this identity';
    var requireConfirm = opts.requireConfirm !== false;
    var copyBtn = '<button type="button" class="ui-btn ui-btn--quiet ui-btn--sm identity-phrase-copy" id="recovery-backup-copy"' + (requireConfirm ? '' : ' hidden') + '>' + RS.kit.icon('copy') + 'Copy phrase</button>';
    var body = requireConfirm
        ? '<p>These ' + RECOVERY_PHRASE_WORDS + ' words are the only way back into ' + escapeHtml(name) + ' if this device is lost. Write them down in order and keep them offline.</p>' +
            recoveryPhraseHtml(words, false) + copyBtn +
            '<label class="ui-check-row"><input type="checkbox" id="recovery-backup-confirm"><span class="ui-check">' + RS.kit.icon('check') + '</span><span>I’ve written down the ' + RECOVERY_PHRASE_WORDS + ' words and stored them safely.</span></label>'
        : '<p>These ' + RECOVERY_PHRASE_WORDS + ' words restore ' + escapeHtml(name) + ' on any device. Write them down in order and keep them offline.</p>' +
            recoveryPhraseHtml(words, true, 'recovery-backup-shell') + copyBtn;
    var done = false;
    function finish() {
        if (done) return;
        done = true;
        if (typeof onDone === 'function') onDone();
    }
    var built = RS.kit.dialog({
        title: opts.title || (requireConfirm ? 'Write down your recovery phrase' : 'Recovery phrase'),
        size: 'md',
        locked: requireConfirm,
        body: body,
        foot: '<button type="button" class="ui-btn ui-btn--primary" id="recovery-backup-done"' + (requireConfirm ? ' disabled' : '') + '>' + escapeHtml(opts.button || (requireConfirm ? 'Continue' : 'Done')) + '</button>',
        onClose: finish
    });
    var cover = built.q('.ui-phrase-cover');
    var copy = built.q('#recovery-backup-copy');
    if (cover) {
        cover.addEventListener('click', function() {
            var shell = built.q('#recovery-backup-shell');
            shell.classList.remove('is-covered');
            shell.querySelector('ol').removeAttribute('aria-hidden');
            cover.remove();
            copy.hidden = false;
        });
        cover.focus();
    }
    copy.addEventListener('click', function() {
        RS.copySecret(mnemonic).then(function(ok) {
            if (ok) showSecretCopiedToast('Recovery phrase');
            else showToast('Could not copy the recovery phrase', 'toast-error', 2000);
        });
    });
    var confirm = built.q('#recovery-backup-confirm');
    var doneBtn = built.q('#recovery-backup-done');
    if (confirm) confirm.addEventListener('change', function() { doneBtn.disabled = !confirm.checked; });
    doneBtn.addEventListener('click', function() {
        if (!doneBtn.disabled) built.close(true);
    });
}
window.showRecoveryPhraseBackup = showRecoveryPhraseBackup;

// Re-display the active software identity's recovery phrase, with its PIN or
// after a confirmation. Hardware identities never reach here (no stored phrase).
function viewRecoveryPhrase(target) {
    if (!target || target.is_hardware) return;
    var name = identityDisplayName(target);
    authorizeIdentityKey(target, {
        title: 'Show the recovery phrase?',
        message: 'Anyone who sees these ' + RECOVERY_PHRASE_WORDS + ' words can take over ' + name + '. Check that no one can see your screen.',
        confirmText: 'Show phrase',
        pinLabel: name + '’s PIN',
        pinHelp: 'This identity is locked with a PIN.'
    }, function(auth) {
        return RS.invoke('reveal_identity_mnemonic', { args: Object.assign({ hash: target.hash }, auth) });
    }).then(function(data) {
        if (data) showRecoveryPhraseBackup(data.mnemonic, null, { name: name, requireConfirm: false });
    }).catch(function(err) {
        showToast((err && err.message) || 'Could not reveal phrase', 'toast-error', 3000);
    });
}
window.viewRecoveryPhrase = viewRecoveryPhrase;

function viewActiveRecoveryPhrase() {
    var active = activeIdentity();
    if (!active) {
        showPreConditionToast('No active identity to reveal');
        return;
    }
    if (active.is_hardware) {
        showPreConditionToast('Hardware-key identities do not have a recovery phrase on this device');
        return;
    }
    if (!active.has_mnemonic) {
        showPreConditionToast('No recovery phrase is available for this identity');
        return;
    }
    viewRecoveryPhrase(active);
}
window.viewActiveRecoveryPhrase = viewActiveRecoveryPhrase;

function openRestorePhraseModal(fromSetup) {
    showIdentityModal('Restore from a recovery phrase',
        '<div class="ui-field"><label class="ui-label" for="restore-phrase-input">Recovery phrase</label>' +
            '<textarea id="restore-phrase-input" class="ui-input ui-input--mono" rows="3" autocomplete="off" autocapitalize="off" spellcheck="false" placeholder="Your ' + RECOVERY_PHRASE_WORDS + ' words, separated by spaces" aria-describedby="restore-phrase-count"></textarea>' +
            '<p class="ui-help" id="restore-phrase-count">0 of ' + RECOVERY_PHRASE_WORDS + ' words</p></div>' +
        identityField('restore-phrase-nickname', 'Name', { maxlength: 32, placeholder: 'Optional' }) +
        identityPasscodeOptionHtml('restore-phrase') +
        identityErrorHtml('restore-phrase-error'),
        function() {
            var ta = document.getElementById('restore-phrase-input');
            var phrase = (ta ? ta.value : '').trim().replace(/\s+/g, ' ');
            var nickname = document.getElementById('restore-phrase-nickname').value.trim();
            var errEl = document.getElementById('restore-phrase-error');
            var passcodeOption = readIdentityPasscodeOption('restore-phrase');
            if (!passcodeOption) return;
            var passcode = passcodeOption.passcode;
            var count = phrase ? phrase.split(' ').length : 0;
            if (count !== RECOVERY_PHRASE_WORDS) {
                if (errEl) { errEl.textContent = 'A recovery phrase is exactly ' + RECOVERY_PHRASE_WORDS + ' words.'; errEl.style.display = ''; }
                return;
            }
            if (errEl) errEl.style.display = 'none';
            return RS.invoke('restore_seed_identity', {
                args: { phrase: phrase, nickname: nickname }
            }).then(function(data) {
                var hash = data && (data.hash || data.identity_hash);
                var protect = passcode ? protectIdentityWithPasscode(hash, passcode).catch(function(err) {
                    var msg = identityPinError(err, 'Could not encrypt identity with PIN');
                    showToast('Identity restored, but the PIN could not be set: ' + msg, 'toast-error', 7000);
                }) : Promise.resolve();
                return protect.then(function() {
                    closeIdentityModal();
                    if (fromSetup) {
                        // Restore mirrors the import completion path: the restored
                        // identity is active when setup has no identity, so restart
                        // the core and transition to the connecting screen.
                        if (typeof completeSetupAfterIdentityImport === 'function') {
                            completeSetupAfterIdentityImport(data);
                        } else {
                            reloadToAppLanding();
                        }
                        return;
                    }
                    showToast('Identity restored', 'toast-success', 3000);
                    loadIdentities();
                });
            }).catch(function(err) {
                var msg = (err && err.message) ? err.message : 'Failed to restore identity';
                if (errEl) { errEl.textContent = msg; errEl.style.display = ''; }
                else showToast(msg, 'toast-error', 4000);
            });
        }, '', 'Restore'
    );

    var ta = document.getElementById('restore-phrase-input');
    var countEl = document.getElementById('restore-phrase-count');
    if (ta && countEl) {
        ta.addEventListener('input', function() {
            var words = ta.value.trim().split(/\s+/).filter(Boolean);
            countEl.textContent = words.length + ' of ' + RECOVERY_PHRASE_WORDS + ' words';
        });
    }
    bindIdentityPasscodeOption('restore-phrase');
}

function importIdentity() {
    chooseIdentityImportFormat().then(function(format) {
        if (!format) {
            resetPendingIdentityImport();
            return;
        }
        window._identityImportFormat = format;
        if (format === 'phrase') {
            // Recovery-phrase restore is text input, not a file — open its modal.
            openRestorePhraseModal(!!window._identityImportFromSetup);
            return;
        }
        if (format === 'ratspeak') {
            openRatspeakBackupImportPinModal(function(pin) {
                window._identityImportPasscode = pin;
                startIdentityFileImport(format);
            });
            return;
        }
        startIdentityFileImport(format);
    });
}

function openRatspeakBackupImportPinModal(onPin) {
    var body = '<p>Enter the PIN that was set when this .rsi backup was exported. The imported identity is locked on this device with the same PIN.</p>' +
        backupPinFieldsHtml('identity-import-backup', false, 'backup PIN');
    showIdentityModal('Unlock the backup', body, function() {
        var pin = readBackupPin('identity-import-backup', false);
        if (!pin) return;
        closeIdentityModal();
        onPin(pin);
    }, '', 'Unlock');
}

function startIdentityFileImport(format) {
    if (hasAndroidIdentityImportBridge()) {
        openIdentityBackupWithAndroid().then(function(result) {
            return handleImportBackupPayload(
                result.fileName,
                result.fileSize,
                result.backupBase64,
                format
            );
        }).catch(function(err) {
            resetPendingIdentityImport();
            if (!(err && err.cancelled)) {
                showToast(err && err.message ? err.message : 'Could not import the identity', 'toast-error', 4000);
            }
        });
        return;
    }

    var fileInput = document.getElementById('identity-file-input');
    if (fileInput) {
        if (format === 'ratspeak') {
            fileInput.accept = '.rsi,application/octet-stream,application/json';
        } else {
            fileInput.accept = '.identity,.key,.bin,.txt,application/octet-stream,text/plain';
        }
        fileInput.value = '';
        fileInput.click();
    }
}

function handleImportFile(file) {
    if (!file) return;

    file.arrayBuffer().then(function(buf) {
        var bytes = new Uint8Array(buf);
        return handleImportBackupPayload(
            file.name,
            file.size,
            bytesToBase64(bytes),
            window._identityImportFormat || 'ratspeak'
        );
    }).catch(function(err) {
        resetPendingIdentityImport();
        showToast(err && err.message ? err.message : 'Could not import the identity', 'toast-error', 4000);
    });
}

function handleImportBackupPayload(fileName, fileSize, b64, expectedFormat) {
    if (!b64) return Promise.reject(new Error('Identity import is empty'));
    var fromSetup = !!window._identityImportFromSetup;
    var importFormat = expectedFormat || window._identityImportFormat || 'ratspeak';
    var importPasscode = importFormat === 'ratspeak' ? (window._identityImportPasscode || '') : '';
    if (importFormat === 'ratspeak' && !importPasscode) {
        resetPendingIdentityImport();
        showToast('Enter the backup PIN to continue', 'toast-warning', 3000);
        return Promise.resolve();
    }
    return RS.invoke('api_preview_identity_import_base64', {
        args: { key: b64, nickname: '', passcode: importPasscode }
    }).then(function(preview) {
        var isRatspeakBackup = preview.format === 'ratspeak.identity.v2' || preview.format === 'ratspeak.identity.v1';
        if (importFormat === 'ratspeak' && !isRatspeakBackup) {
            resetPendingIdentityImport();
            showToast('Choose Reticulum Identity Key to import this file.', 'toast-warning', 4000);
            return;
        }
        if (importFormat === 'reticulum' && isRatspeakBackup) {
            resetPendingIdentityImport();
            showToast('Choose Ratspeak Identity Backup to import this file.', 'toast-warning', 4000);
            return;
        }
        var duplicate = false;
        for (var i = 0; i < identityList.length; i++) {
            if (identityList[i].hash === preview.identity_hash) duplicate = true;
        }
        if (duplicate) {
            resetPendingIdentityImport();
            showToast('This identity is already on this device', 'toast-info', 3000);
            return;
        }
        var activateHtml = fromSetup ? '' :
            '<label class="ui-check-row"><input type="checkbox" id="identity-import-activate"><span class="ui-check">' + RS.kit.icon('check') + '</span><span>Switch to it after the import</span></label>';
        showIdentityModal('Import identity',
            '<p>' + escapeHtml(fileName || 'Identity file') + ' \u00b7 ' + (fileSize || 0) + ' bytes</p>' +
            personDlHtml([
                ['LXMF address', RS.kit.hash.copy(preview.lxmf_hash || '', { form: 'fit', noun: 'LXMF address' })],
                ['Identity hash', RS.kit.hash.copy(preview.identity_hash || '', { form: 'fit', noun: 'Identity hash' })]
            ]) +
            identityField('identity-modal-nickname', 'Name', { maxlength: 32, placeholder: 'Optional' }) +
            activateHtml,
            function() {
                var nickname = document.getElementById('identity-modal-nickname').value.trim();
                var activateInput = document.getElementById('identity-import-activate');
                var activate = fromSetup || !!(activateInput && activateInput.checked);
                return RS.invoke('api_import_identity_base64', {
                    args: { key: b64, nickname: nickname, passcode: importPasscode },
                }).then(function(data) {
                    var protect = (importFormat === 'ratspeak' && importPasscode && data && data.hash)
                        ? protectIdentityWithPasscode(data.hash, importPasscode).catch(function(err) {
                            var msg = (err && err.message) ? err.message : 'Could not encrypt identity on this device';
                            showToast('Identity imported, but the local PIN could not be set: ' + msg, 'toast-error', 7000);
                        })
                        : Promise.resolve();
                    return protect.then(function() {
                        showToast('Identity imported', 'toast-success', 3000);
                        closeIdentityModal();
                        loadIdentities();
                        if (fromSetup || window._identityImportFromSetup) {
                            resetPendingIdentityImport();
                            if (typeof completeSetupAfterIdentityImport === 'function') {
                                completeSetupAfterIdentityImport(data);
                            } else {
                                reloadToAppLanding();
                            }
                            return;
                        }
                        window._identityImportFormat = null;
                        window._identityImportPasscode = null;
                        if (activate && data && data.hash) switchToIdentity(data.hash);
                    });
                }).catch(function(err) {
                    resetPendingIdentityImport();
                    showToast(err && err.message ? err.message : 'Could not import the identity', 'toast-error', 3000);
                });
            }
        );
    });
}

function exportActiveIdentity() {
    var active = activeIdentity();
    exportIdentityBackup((active && active.hash) || activeIdentityHash);
}

// `auth` carries the identity's own authorization, and for a backup also the
// backup's `passcode`.
function exportIdentityPayload(hash, format, auth) {
    var args = Object.assign({ hashHex: hash }, auth);
    if (format === 'reticulum') {
        return RS.invoke('api_export_identity_reticulum_base64', args).then(function(data) {
            return {
                bytes: base64ToBytes(data.data_base64),
                base64: data.data_base64,
                fileName: data.file_name || (hash.substring(0, 16) + '-reticulum-identity.identity'),
                mimeType: 'application/octet-stream',
                label: 'Reticulum identity file'
            };
        });
    }
    if (format === 'reticulum-base32') {
        return RS.invoke('api_export_identity_reticulum_base32', args).then(function(data) {
            return {
                bytes: base64ToBytes(data.data_base64),
                base64: data.data_base64,
                fileName: data.file_name || (hash.substring(0, 16) + '-reticulum-identity-key-base32.txt'),
                mimeType: 'text/plain',
                label: 'Reticulum base32 key'
            };
        });
    }
    return RS.invoke('api_export_identity_backup_base64', args).then(function(data) {
        return {
            bytes: base64ToBytes(data.backup_base64),
            base64: data.backup_base64,
            fileName: data.file_name || (hash.substring(0, 16) + '-ratspeak-identity.rsi'),
            mimeType: 'application/octet-stream',
            label: 'Ratspeak identity backup'
        };
    });
}

function exportWarningForFormat(format, targetName) {
    if (format === 'reticulum') {
        return 'This exports the unencrypted private Reticulum identity key for ' + targetName + '. Anyone with this file can use the identity. Ratspeak messages and settings are not included.';
    }
    if (format === 'reticulum-base32') {
        return 'This exports the unencrypted private Reticulum identity key for ' + targetName + ' as base32 text. Anyone with this text can use the identity. Ratspeak messages and settings are not included.';
    }
    return 'This exports a PIN-encrypted Ratspeak identity backup for ' + targetName + '. Anyone with the file and PIN can use the identity. It includes identity metadata, but not messages or app settings.';
}

function saveIdentityExportPayload(payload) {
    return saveBytesToUserFile(payload.bytes, payload.fileName, payload.mimeType, payload.base64)
        .then(function(result) {
            if (result) result.label = payload.label;
            return result;
        }).then(function(result) {
            if (!result) return;
            if (result.method === 'android-document' || result.status === 'saved') {
                showToast((result.label || 'Identity export') + ' saved', 'toast-success', 3000);
            } else if (result.status === 'failed') {
                showToast('Could not save ' + result.fileName, 'toast-error', 5000);
            } else if (result.status !== 'cancelled') {
                showToast('Save prompt opened for ' + result.fileName, 'toast-info', 4000);
            }
        });
}

function handleIdentityExportError(err) {
    if (!(err && err.cancelled)) {
        showToast(err && err.message ? err.message : 'Could not export the identity', 'toast-error', 3000);
    }
}

function openEncryptedIdentityExportModal(hash, target) {
    var protectedIdentity = !!(target && target.passcode_protected);
    var body = '<p>The backup PIN locks the .rsi file. You need it to import this identity on another device.</p>' +
        (protectedIdentity
            ? identityField('identity-export-identity-pin', 'Identity PIN', { type: 'password', maxlength: 128 })
            : '') +
        backupPinFieldsHtml('identity-export-backup', true, 'backup PIN');
    showIdentityModal('Export a Ratspeak backup', body, function() {
        var errEl = document.getElementById('identity-export-backup-pin-error');
        function fail(text) { if (errEl) { errEl.textContent = text; errEl.style.display = ''; } }
        // Opening this sheet and choosing Export is the confirmation.
        var auth = { confirmed: true };
        if (protectedIdentity) {
            var identityPin = document.getElementById('identity-export-identity-pin').value;
            if (!identityPin) { fail("Enter this identity's PIN."); return; }
            auth = { identityPasscode: identityPin };
        }
        var pin = readBackupPin('identity-export-backup', true);
        if (!pin) return;
        auth.passcode = pin;
        return exportIdentityPayload(hash, 'ratspeak', auth).then(function(payload) {
            closeIdentityModal();
            return saveIdentityExportPayload(payload).catch(handleIdentityExportError);
        }, function(err) {
            fail(identityPinError(err, 'Could not export the identity'));
        });
    }, '', 'Export');
}

function exportIdentityBackup(hash) {
    if (!hash) {
        showPreConditionToast('No active identity to export');
        return;
    }
    var target = identityByHash(hash);
    if (target && target.is_hardware) {
        showPreConditionToast('Hardware-key identities cannot be exported — the private key never leaves the device');
        return;
    }
    var targetName = target ? identityDisplayName(target) : 'Identity';
    chooseIdentityExportFormat(targetName).then(function(format) {
        if (!format) return null;
        if (format === 'ratspeak') {
            openEncryptedIdentityExportModal(hash, target);
            return null;
        }
        return authorizeIdentityKey(target, {
            title: 'Export the private key?',
            message: exportWarningForFormat(format, targetName),
            confirmText: 'Export',
            danger: true
        }, function(auth) { return exportIdentityPayload(hash, format, auth); });
    }).then(function(payload) {
        if (!payload) return;
        return saveIdentityExportPayload(payload);
    }).catch(handleIdentityExportError);
}


// Add or change a passcode (at-rest encryption) on a software identity.
function openSetPasscodeModal(target, isChange) {
    var body =
        '<p>' + escapeHtml(identityDisplayName(target)) + ' is locked on this device and asks for the PIN each time Ratspeak opens. A forgotten PIN can’t be recovered; keep your ' + RECOVERY_PHRASE_WORDS + '-word phrase.</p>' +
        (isChange ? identityField('passcode-current', 'Current PIN', { type: 'password', maxlength: 128 }) : '') +
        identityField('passcode-new', isChange ? 'New PIN' : 'PIN', { type: 'password', maxlength: 128, help: 'At least 6 characters.' }) +
        identityField('passcode-confirm', 'Confirm PIN', { type: 'password', maxlength: 128 }) +
        identityErrorHtml('passcode-error');
    showIdentityModal(isChange ? 'Change PIN' : 'Set a PIN', body, function() {
        var cur = isChange ? document.getElementById('passcode-current').value : '';
        var pw = document.getElementById('passcode-new').value;
        var confirm = document.getElementById('passcode-confirm').value;
        var errEl = document.getElementById('passcode-error');
        if (pw.length < 6) {
            if (errEl) { errEl.textContent = 'PIN must be at least 6 characters.'; errEl.style.display = ''; }
            return;
        }
        if (pw !== confirm) {
            if (errEl) { errEl.textContent = "PINs don't match."; errEl.style.display = ''; }
            return;
        }
        var args = { hash: target.hash, passcode: pw };
        if (isChange) args.current = cur;
        return RS.invoke('set_identity_passcode', { args: args }).then(function() {
            closeIdentityModal();
            showToast(isChange ? 'PIN changed' : 'PIN set', 'toast-success', 3000);
            loadIdentities();
        }).catch(function(err) {
            if (errEl) { errEl.textContent = identityPinError(err, 'Could not set PIN'); errEl.style.display = ''; }
        });
    }, '', isChange ? 'Change PIN' : 'Set PIN');
    // Turning the PIN off starts from changing it.
    var cancel = isChange && document.getElementById('identity-modal-cancel');
    if (cancel) cancel.insertAdjacentHTML('beforebegin', '<button type="button" class="ui-btn ui-btn--quiet identity-pin-remove" id="identity-pin-remove">Remove PIN</button>');
    var remove = isChange && document.getElementById('identity-pin-remove');
    if (remove) remove.addEventListener('click', function() {
        closeIdentityModal();
        openRemovePasscodeModal(target);
    });
}

function openHardwareChangePinModal(target) {
    var name = identityDisplayName(target);
    var body =
        '<p>This changes the YubiKey PIN for ' + escapeHtml(name) + '. Keep the key plugged in until it finishes.</p>' +
        identityField('hardware-pin-current', 'Current PIN', { type: 'password', maxlength: 8, inputmode: 'numeric' }) +
        identityField('hardware-pin-new', 'New PIN', { type: 'password', maxlength: 8, inputmode: 'numeric', help: '6 to 8 characters.' }) +
        identityField('hardware-pin-confirm', 'Confirm PIN', { type: 'password', maxlength: 8, inputmode: 'numeric' }) +
        identityErrorHtml('hardware-pin-error');
    showIdentityModal('Change PIN', body, function() {
        var current = document.getElementById('hardware-pin-current').value;
        var pin = document.getElementById('hardware-pin-new').value;
        var confirm = document.getElementById('hardware-pin-confirm').value;
        var errEl = document.getElementById('hardware-pin-error');
        if (current.length < 6 || current.length > 8) {
            if (errEl) { errEl.textContent = 'Current PIN must be 6-8 characters.'; errEl.style.display = ''; }
            return;
        }
        if (pin.length < 6 || pin.length > 8) {
            if (errEl) { errEl.textContent = 'New PIN must be 6-8 characters.'; errEl.style.display = ''; }
            return;
        }
        if (pin !== confirm) {
            if (errEl) { errEl.textContent = "PINs don't match."; errEl.style.display = ''; }
            return;
        }
        return RS.invoke('hw_change_pin', { hash: target.hash, currentPin: current, newPin: pin }).then(function() {
            closeIdentityModal();
            showToast('PIN changed', 'toast-success', 3000);
        }).catch(function(err) {
            if (errEl) { errEl.textContent = identityPinError(err, 'Could not change PIN'); errEl.style.display = ''; }
        });
    }, '', 'Change PIN');
}

function openRemovePasscodeModal(target) {
    showIdentityModal('Remove the PIN?',
        '<p>' + escapeHtml(identityDisplayName(target)) + ' is unlocked on this device and no longer asks for a PIN when Ratspeak opens.</p>' +
        identityField('passcode-remove-input', 'PIN', { type: 'password', maxlength: 128 }) +
        identityErrorHtml('passcode-error'),
        function() {
            var pw = document.getElementById('passcode-remove-input').value;
            var errEl = document.getElementById('passcode-error');
            if (!pw) {
                if (errEl) { errEl.textContent = 'Enter your PIN.'; errEl.style.display = ''; }
                return;
            }
            return RS.invoke('remove_identity_passcode', { args: { hash: target.hash, passcode: pw } }).then(function() {
                closeIdentityModal();
                showToast('PIN removed', 'toast-success', 3000);
                loadIdentities();
            }).catch(function(err) {
                if (errEl) { errEl.textContent = identityPinError(err, 'Could not remove PIN'); errEl.style.display = ''; }
            });
        }, 'danger', 'Remove PIN'
    );
}



function deleteIdentityByHash(hash) {
    if (!hash) {
        showPreConditionToast('Select an identity to delete');
        return;
    }
    var target = identityByHash(hash);
    if (!target) {
        showPreConditionToast('Identity not found');
        return;
    }
    if (target.is_active && identityList.length <= 1) {
        showPreConditionToast('Cannot remove the active identity');
        return;
    }
    if (isOriginalIdentity(hash)) {
        showPreConditionToast('Cannot remove the original identity');
        return;
    }

    if (target.is_hardware) {
        removeHardwareIdentity(target);
        return;
    }

    var name = identityDisplayName(target);
    var cascade = false;
    // One dialog: the identity goes, and its data stays (the default) or goes with it.
    var built = RS.kit.dialog({
        title: 'Delete ' + name + '?',
        body: '<p>The identity is removed from this device. Without a backup or its recovery phrase, it can’t be restored.</p>' +
            '<div class="ui-choices" role="radiogroup" aria-label="Its data">' +
                '<button type="button" class="ui-menu-item" role="radio" aria-checked="true" data-cascade="0"><span class="lbl">Keep its contacts and messages<small>They come back if you import this identity again.</small></span>' + RS.kit.icon('check', 'check') + '</button>' +
                '<button type="button" class="ui-menu-item" role="radio" aria-checked="false" data-cascade="1"><span class="lbl">Delete them too<small>Contacts, messages, and games for ' + escapeHtml(name) + ' are erased.</small></span></button>' +
            '</div>',
        foot: '<button type="button" class="ui-btn ui-btn--quiet ui-desk-only" data-ui-close>Cancel</button>' +
            '<button type="button" class="ui-btn ui-btn--danger ui-btn--primary" id="identity-delete-confirm">Delete identity</button>'
    });
    built.dialog.addEventListener('click', function(e) {
        var radio = e.target.closest('[data-cascade]');
        if (!radio) return;
        cascade = radio.getAttribute('data-cascade') === '1';
        built.dialog.querySelectorAll('[data-cascade]').forEach(function(btn) {
            var on = btn === radio;
            btn.setAttribute('aria-checked', on ? 'true' : 'false');
            var check = btn.querySelector('.ui-ico.check');
            if (on && !check) btn.insertAdjacentHTML('beforeend', RS.kit.icon('check', 'check'));
            if (!on && check) check.remove();
        });
    });
    var confirmBtn = built.q('#identity-delete-confirm');
    confirmBtn.addEventListener('click', function() {
        confirmBtn.disabled = true;
        var deletePromise;
        if (target.is_active) {
            var firstRemaining = null;
            for (var j = 0; j < identityList.length; j++) {
                if (identityList[j].hash !== hash) {
                    firstRemaining = identityList[j];
                    break;
                }
            }
            if (!firstRemaining) {
                built.close(null);
                showPreConditionToast('No other identity to switch to');
                return;
            }
            deletePromise = RS.invoke('api_activate_identity', { hashHex: firstRemaining.hash })
                .then(function() {
                    activeIdentityHash = firstRemaining.hash;
                    return RS.invoke('api_delete_identity', { hashHex: hash, cascade: cascade });
                });
        } else {
            deletePromise = RS.invoke('api_delete_identity', { hashHex: hash, cascade: cascade });
        }

        deletePromise
            .then(function() {
                built.close(true);
                showToast('Identity deleted', 'toast-success', 3000);
                selectedIdentityHash = null;
                _identityPhoneDetailOpen = false;
                loadIdentities();
            })
            .catch(function(err) {
                confirmBtn.disabled = false;
                showToast(err && err.message ? err.message : 'Could not delete the identity', 'toast-error', 3000);
            });
    });
}

// The identity dialogs' shared shell: one kit dialog at a time, with the confirm
// button that shows the work while it runs. confirmLabel names the action.
var _identityModal = null;
function showIdentityModal(title, bodyHtml, onConfirm, confirmClass, confirmLabel) {
    if (_identityModal) _identityModal.close(null);
    var danger = !!(confirmClass && (confirmClass.indexOf('danger') !== -1 || confirmClass.indexOf('error') !== -1));
    var baseLabel = confirmLabel || (title.indexOf('Delete') !== -1 ? 'Delete' :
        (title.indexOf('Remove') !== -1 ? 'Remove' :
        (title.indexOf('Import') !== -1 ? 'Import' :
        (title.indexOf('Restore') !== -1 ? 'Restore' :
        (title.indexOf('Export') !== -1 ? 'Export' :
        (title.indexOf('Unlock') !== -1 ? 'Unlock' : 'Create'))))));
    var built = RS.kit.dialog({
        title: title,
        className: 'identity-modal',
        body: '<div class="identity-modal-body" id="identity-modal-body">' + bodyHtml + '</div>',
        foot: '<button type="button" class="ui-btn ui-btn--quiet ui-desk-only" id="identity-modal-cancel">Cancel</button>' +
            '<button type="button" class="ui-btn ui-btn--primary' + (danger ? ' ui-btn--danger' : '') + '" id="identity-modal-confirm">' + escapeHtml(baseLabel) + '</button>',
        onClose: function() {
            if (_identityModal === built) _identityModal = null;
            if (/^Import/.test(title)) resetPendingIdentityImport();
        }
    });
    _identityModal = built;
    var confirmBtn = built.q('#identity-modal-confirm');
    confirmBtn.dataset.baseLabel = baseLabel;
    built.q('#identity-modal-cancel').addEventListener('click', closeIdentityModal);
    confirmBtn.onclick = function() {
        if (confirmBtn.disabled) return;
        confirmBtn.disabled = true;
        confirmBtn.textContent = 'Working…';
        var restore = function() {
            confirmBtn.disabled = false;
            confirmBtn.textContent = confirmBtn.dataset.baseLabel || baseLabel;
        };
        var result;
        try {
            result = onConfirm && onConfirm();
        } catch (e) {
            restore();
            throw e;
        }
        if (result && typeof result.then === 'function') {
            var done = false;
            var settle = function() { if (!done) { done = true; restore(); } };
            result.then(settle, settle);
        } else {
            restore();
        }
    };
    built.dialog.addEventListener('keydown', function(e) {
        if (e.key === 'Enter' && e.target.tagName === 'INPUT' && e.target.type !== 'checkbox') {
            e.preventDefault();
            confirmBtn.click();
        }
    });
}

function closeIdentityModal() {
    if (_identityModal) _identityModal.close(null);
}

document.addEventListener('DOMContentLoaded', function() {
    var fileInput = document.getElementById('identity-file-input');
    if (fileInput) {
        fileInput.addEventListener('change', function() {
            if (this.files && this.files[0]) {
                handleImportFile(this.files[0]);
                this.value = '';
            }
        });
        fileInput.addEventListener('cancel', function() {
            resetPendingIdentityImport();
        });
    }

});

RS.listen('identity_switching', function() {
    window._identitySwitchInProgress = true;
    // The previous identity's contacts leave with it; the next start re-sends its own.
    if (typeof lxmfContacts !== 'undefined') {
        lxmfContacts = [];
        if (typeof renderStandaloneContactList === 'function') renderStandaloneContactList();
    }
    if (typeof clearNetworkInterfaceCaches === 'function') {
        clearNetworkInterfaceCaches({ render: true });
    }
    if (typeof _clearConnectTimeout === 'function') _clearConnectTimeout();
    if (typeof clearConnectPublicPending === 'function') clearConnectPublicPending();
});

// A switch that did not finish: the previous identity is back, or none is.
RS.listen('identity_switch_failed', function(data) {
    window._identitySwitchInProgress = false;
    if (!_switchRequestsPending && data && data.error) _identityNoteSwitchFailure(data.hash, data.error);
    loadIdentities();
    if (typeof loadConversations === 'function') loadConversations();
});

// Drop every cached conversation: after an identity switch or a bulk delete,
// nothing cached may be merged back into a fresh fetch.
function resetConversationCaches() {
    if (typeof lxmfConversation !== 'undefined') lxmfConversation = [];
    if (typeof _conversationCache !== 'undefined') {
        for (var k in _conversationCache) delete _conversationCache[k];
    }
    if (typeof _conversationCacheSizes !== 'undefined') _conversationCacheSizes = {};
    if (typeof _conversationCacheBytes !== 'undefined') _conversationCacheBytes = 0;
    if (typeof _cacheLru !== 'undefined') _cacheLru = [];
}

// A bulk delete announces what it removed before it re-sends the lists.
// Events arrive in order, so this drops only what the window held before it.
RS.listen('identity_data_cleared', function(data) {
    if (data && data.messages) {
        if (typeof _messagesClearedCount !== 'undefined') _messagesClearedCount += 1;
        resetConversationCaches();
        if (typeof clearReplyTarget === 'function') clearReplyTarget();
        if (typeof _resetConversationList === 'function') _resetConversationList();
    }
    if (data && data.contacts && typeof lxmfContacts !== 'undefined') {
        lxmfContacts = [];
        if (typeof renderStandaloneContactList === 'function') renderStandaloneContactList();
    }
    if (typeof renderConversation === 'function') renderConversation();
    if (typeof _reloadPeersAfterCacheClear === 'function') _reloadPeersAfterCacheClear(true);
});

RS.listen('identity_switched', function(data) {
    // Suppress the redundant loadConversations() that lxmf_identity triggers —
    // emit_initial_state fires lxmf_identity right after identity_switched.
    window._identitySwitchInProgress = true;

    activeIdentityHash = data.hash;
    selectedIdentityHash = data.hash;

    if (data.lxmf_hash && typeof updateHeaderIdentity === 'function') {
        updateHeaderIdentity(
            data.lxmf_hash,
            data.display_name || '',
            typeof profileStatusFromPayload === 'function' ? profileStatusFromPayload(data) : null
        );
    }

    loadIdentities();

    // Clear identity-scoped frontend state so the old identity's data
    // doesn't leak. PeersCache rehydrates from the new snapshot on activation.
    // Contacts left when the switch began, and the new identity's start has
    // already sent its own.
    resetConversationCaches();

    if (typeof _resetConversationSession === 'function') _resetConversationSession('identity_replaced');
    else if (typeof lxmfActiveContact !== 'undefined') lxmfActiveContact = null;
    if (typeof lxmfPendingFile !== 'undefined') lxmfPendingFile = null;
    if (typeof lxmfIdentity !== 'undefined') lxmfIdentity = null;
    if (typeof _ghostConversationHash !== 'undefined') _ghostConversationHash = null;
    if (typeof _replyTarget !== 'undefined') _replyTarget = null;
    if (typeof _msgReactions !== 'undefined') _msgReactions = {};
    if (typeof _lxmfDrafts !== 'undefined') _lxmfDrafts = {};

    if (typeof lxmfIdentityHash !== 'undefined') lxmfIdentityHash = data.hash;

    if (typeof events !== 'undefined') events = [];
    if (typeof activityEvents !== 'undefined') activityEvents = [];
    if (typeof activityResetReveals === 'function') activityResetReveals();

    var msgList = document.getElementById('lxmf-messages');
    if (msgList) msgList.innerHTML = '<div class="lxmf-empty">Select a contact to view conversation.</div>';
    var chatHeader = document.getElementById('lxmf-chat-header');
    if (chatHeader) chatHeader.style.display = 'none';

    if (typeof _resetConversationList === 'function') _resetConversationList();
    if (typeof renderStandaloneContactList === 'function') renderStandaloneContactList();
    if (typeof closeFabContactPicker === 'function') closeFabContactPicker();
    if (typeof loadConversations === 'function') loadConversations();

    if (typeof renderMergedConnections === 'function') renderMergedConnections();
    if (typeof refreshConnectPublicServers === 'function') refreshConnectPublicServers(null, { force: true });

    if (typeof gamesTabClear === 'function') gamesTabClear();

    if (typeof renderActivityFeed === 'function') renderActivityFeed();
    if (typeof renderLog === 'function') renderLog();

    setTimeout(function() { window._identitySwitchInProgress = false; }, 2000);

    showToast('Identity switched', 'toast-success', 3000);
});

// ---------------------------------------------------------------------------
// Hardware Key (YubiKey/Nitrokey PIV) identity flow
// ---------------------------------------------------------------------------

var HW_BADGE_ICON = '<svg class="identity-hardware-badge-icon" viewBox="0 0 24 24" aria-hidden="true"><path d="M2.6 17.4A2 2 0 0 0 2 18.8V21a1 1 0 0 0 1 1h3a1 1 0 0 0 1-1v-1a1 1 0 0 1 1-1h1a1 1 0 0 0 1-1v-1a1 1 0 0 1 1-1h.2a2 2 0 0 0 1.4-.6l.8-.8A6.5 6.5 0 1 0 9.4 10.6z"/><circle cx="16.5" cy="7.5" r="1.5"/></svg>';

var HW_DETECT_ERROR_COPY = 'No YubiKey found. Connect a YubiKey 5 with firmware 5.7.0 or newer.';

// Wizard state. `mnemonic` is held here only — never persisted, logged, or
// echoed to storage. Cleared on close/finish.
var _hwCtx = null;

var HW_STEP_IDS = [
    'hw-step-detect', 'hw-step-mode', 'hw-step-pin', 'hw-step-working',
    'hw-step-mnemonic', 'hw-step-restore', 'hw-step-import'
];

function _hwSetTitle(text) {
    var el = document.getElementById('hw-modal-title');
    if (el) el.textContent = text;
}

function _hwShowStep(stepId) {
    if (_hwCtx) _hwCtx.step = stepId;
    HW_STEP_IDS.forEach(function(id) {
        var el = document.getElementById(id);
        if (el) el.style.display = (id === stepId) ? '' : 'none';
    });
}

function _hwClearSecrets() {
    if (_hwCtx) {
        _hwCtx.pin = null;
        _hwCtx.currentPin = null;
        _hwCtx.mnemonic = null;
        _hwCtx.mnemonicWords = null;
    }
    var grid = document.getElementById('hw-mnemonic-grid');
    if (grid) grid.innerHTML = '';
    ['hw-pin', 'hw-pin-confirm', 'hw-restore-pin', 'hw-restore-pin-confirm', 'hw-restore-phrase'].forEach(function(id) {
        var el = document.getElementById(id);
        if (el) el.value = '';
    });
}

function _hwOpenSheet() {
    var modal = document.getElementById('hw-modal');
    var overlay = document.getElementById('hw-modal-overlay');
    if (!modal || !overlay) return;
    overlay.hidden = false;
    // rs-sheet-shell + open: native Back closes the wizard and clears its secrets.
    modal.classList.add('open');
    modal._ratspeakDismiss = function() { closeHardwareWizard(); return true; };
    if (typeof _trapFocus === 'function' && !modal._focusTrapHandler) _trapFocus(modal);
    try { modal.focus({ preventScroll: true }); } catch (_) {}
}

function closeHardwareWizard() {
    _hwClearSecrets();
    var modal = document.getElementById('hw-modal');
    var overlay = document.getElementById('hw-modal-overlay');
    if (modal) {
        modal.classList.remove('open');
        if (typeof _releaseFocus === 'function') _releaseFocus(modal);
    }
    if (overlay) overlay.hidden = true;
    _hwCtx = null;
}

function _hwReturnToChoice() {
    var fromSetup = !!(_hwCtx && _hwCtx.fromSetup);
    closeHardwareWizard();
    setTimeout(function() {
        openHardwareWizard({ fromSetup: fromSetup });
    }, 0);
}

// Entry point. opts.fromSetup routes completion through the setup restart flow.
function openHardwareWizard(opts) {
    opts = opts || {};
    _hwCtx = { fromSetup: !!opts.fromSetup, device: null, mode: null, pin: null, currentPin: null, nickname: '', mnemonic: null, existing: null, force: false };

    rsChoice({
        title: 'Hardware Key',
        message: 'Use a YubiKey 5+ security key as your identity. The private key is generated on the device and never leaves it.',
        choices: [
            { label: 'Set up a new key', value: 'new', hint: 'Provision a factory-fresh or reset security key.' },
            { label: 'Use an existing key', value: 'import', hint: 'Register a key that is already provisioned.' }
        ]
    }).then(function(choice) {
        if (!choice) { _hwCtx = null; return; }
        if (choice === 'new') _hwBeginDetect('provision');
        else if (choice === 'import') _hwBeginDetect('import');
    });
}
window.openHardwareWizard = openHardwareWizard;

// Detect device, then route to provision-mode or import-nickname step.
function _hwBeginDetect(next) {
    if (!_hwCtx) _hwCtx = { fromSetup: false };
    _hwCtx.afterDetect = next;
    _hwSetTitle('Add a hardware key');
    _hwShowStep('hw-step-detect');
    _hwOpenSheet();
    _hwRunDetect();
}

function _hwRunDetect() {
    var ctx = _hwCtx;
    var textEl = document.getElementById('hw-detect-text');
    var retryBtn = document.getElementById('hw-detect-retry-btn');
    var panel = document.getElementById('hw-detect-panel');
    if (panel) panel.classList.remove('hw-detect-error');
    if (textEl) textEl.textContent = 'Looking for a security key…';
    if (retryBtn) retryBtn.style.display = 'none';

    RS.invoke('hw_detect').then(function(data) {
        if (_hwCtx !== ctx) return;
        data = data || {};
        if (!data.detected || !data.firmware_ok) {
            _hwDetectFailed(data.error || HW_DETECT_ERROR_COPY);
            return;
        }
        _hwCtx.device = data;
        _hwCtx.existing = data.existing || null;
        if (_hwCtx.afterDetect === 'import') _hwShowImportStep();
        else _hwShowModeStep();
    }).catch(function(err) {
        if (_hwCtx !== ctx) return;
        _hwDetectFailed((err && err.message) || HW_DETECT_ERROR_COPY);
    });
}

function _hwDetectFailed(message) {
    var textEl = document.getElementById('hw-detect-text');
    var retryBtn = document.getElementById('hw-detect-retry-btn');
    var panel = document.getElementById('hw-detect-panel');
    if (panel) panel.classList.add('hw-detect-error');
    if (textEl) textEl.textContent = message;
    if (retryBtn) retryBtn.style.display = '';
}

function _hwDeviceSummaryHtml(device) {
    if (!device) return '';
    var model = device.device_type || 'Security key';
    var parts = [];
    if (device.serial) parts.push('Serial ' + device.serial);
    if (device.firmware) parts.push('firmware ' + device.firmware);
    return '<div class="ui-well hw-device"><span class="ui-tile" aria-hidden="true">' + RS.kit.icon('key') + '</span>' +
        '<span class="hw-device-text"><span class="ui-card-title">' + escapeHtml(model) + '</span>' +
            (parts.length ? '<span class="person-status">' + escapeHtml(parts.join(' \u00b7 ')) + '</span>' : '') +
        '</span></div>';
}

function _hwShowModeStep() {
    _hwSetTitle('Add a hardware key');
    var deviceEl = document.getElementById('hw-mode-device');
    if (deviceEl) deviceEl.innerHTML = _hwDeviceSummaryHtml(_hwCtx.device);
    _hwShowStep('hw-step-mode');
}

function _hwShowImportStep() {
    _hwSetTitle('Use a key you set up');
    var deviceEl = document.getElementById('hw-import-device');
    if (deviceEl) deviceEl.innerHTML = _hwDeviceSummaryHtml(_hwCtx.device);
    var err = document.getElementById('hw-import-error');
    if (err) err.style.display = 'none';
    var nick = document.getElementById('hw-import-nickname');
    if (nick) nick.value = '';
    var pinField = document.getElementById('hw-import-pin-field');
    var pinInput = document.getElementById('hw-import-pin');
    var importBtn = document.getElementById('hw-import-btn');
    var needsPin = !!(_hwCtx && _hwCtx.fromSetup);
    var lead = document.getElementById('hw-import-lead');
    if (lead) {
        lead.textContent = needsPin
            ? 'This security key is already provisioned. Enter its current YubiKey PIN to register and unlock it on this device. This does not change the key PIN.'
            : 'This security key is already provisioned. Register it on this device.';
    }
    if (pinField) pinField.style.display = needsPin ? '' : 'none';
    if (pinInput) pinInput.value = '';
    if (importBtn) importBtn.disabled = needsPin;
    _hwShowStep('hw-step-import');
    setTimeout(function() { if (nick && !isMobile()) nick.focus(); }, 120);
}

function _hwShowPinStep() {
    _hwSetTitle('Choose a PIN');
    document.getElementById('hw-pin-nickname').value = _hwCtx.nickname || '';
    document.getElementById('hw-pin').value = '';
    document.getElementById('hw-pin-confirm').value = '';
    var err = document.getElementById('hw-pin-error');
    if (err) err.style.display = 'none';
    _hwUpdatePinContinue();
    _hwShowStep('hw-step-pin');
    setTimeout(function() { var n = document.getElementById('hw-pin-nickname'); if (n && !isMobile()) n.focus(); }, 120);
}

function _hwPinValid(pin) {
    return typeof pin === 'string' && pin.length >= 6 && pin.length <= 8;
}

function _hwUpdatePinContinue() {
    var pin = document.getElementById('hw-pin').value;
    var confirm = document.getElementById('hw-pin-confirm').value;
    var btn = document.getElementById('hw-pin-continue-btn');
    if (btn) btn.disabled = !(_hwPinValid(pin) && pin === confirm);
}

function _hwOverwriteMessage() {
    var existing = _hwCtx && _hwCtx.existing;
    if (existing && existing.on_card_only) {
        return 'This YubiKey already contains keys in the Ratspeak PIV identity slots. Resetting the security key permanently erases those keys. Only continue if you have a backup or are certain they are not needed. Passkeys, FIDO sign-ins, OTP, and other non-PIV features are not affected.';
    }
    var name = (existing && existing.nickname) ? existing.nickname : 'an existing identity';
    return 'This YubiKey already holds "' + name + '". Resetting the security key permanently erases its Ratspeak keys — this cannot be undone unless you saved its 12-word backup phrase. Passkeys, FIDO sign-ins, OTP, and other non-PIV features are not affected.';
}

function _hwConfirmOverwriteIfNeeded(onConfirm, onCancel) {
    if (!_hwCtx || !_hwCtx.existing || _hwCtx.force) {
        if (onConfirm) onConfirm();
        return;
    }
    _hwResetPivWithConfirmation({
        title: 'Reset this security key?',
        message: _hwOverwriteMessage(),
        beforeReset: function() { _hwShowWorking('Resetting your security key…'); }
    }).then(function(reset) {
        if (!reset || !_hwCtx) {
            if (onCancel) onCancel();
            return;
        }
        _hwCtx.existing = null;
        _hwCtx.currentPin = null;
        _hwCtx.force = false;
        if (onConfirm) onConfirm();
    });
}

function _hwChooseProvisionMode(mode) {
    if (!_hwCtx) return;
    _hwCtx.mode = mode;
    _hwConfirmOverwriteIfNeeded(_hwShowPinStep);
}

function _hwPinContinue() {
    var pin = document.getElementById('hw-pin').value;
    var confirm = document.getElementById('hw-pin-confirm').value;
    var err = document.getElementById('hw-pin-error');
    if (!_hwPinValid(pin)) {
        if (err) { err.textContent = 'PIN must be 6–8 characters.'; err.style.display = ''; }
        return;
    }
    if (pin !== confirm) {
        if (err) { err.textContent = "PINs don't match."; err.style.display = ''; }
        return;
    }
    if (err) err.style.display = 'none';
    _hwCtx.pin = pin;
    _hwCtx.currentPin = null;
    _hwCtx.nickname = document.getElementById('hw-pin-nickname').value.trim();
    _hwConfirmOverwriteThenProvision();
}

// Guard against silently overwriting a key that already backs an app identity
// or already has Ratspeak's PIV identity slots occupied.
function _hwConfirmOverwriteThenProvision() {
    _hwConfirmOverwriteIfNeeded(_hwDispatchProvision, _hwShowModeStep);
}

function _hwDispatchProvision() {
    if (_hwCtx.mode === 'recoverable') _hwProvisionRecoverable();
    else _hwProvisionHardwareOnly();
}

function _hwShowWorking(text) {
    var t = document.getElementById('hw-working-text');
    if (t) t.textContent = text || 'Provisioning your security key…';
    _hwSetTitle('Setting up the key');
    _hwShowStep('hw-step-working');
}

function _hwIsPinLockedError(message) {
    message = String(message || '').toLowerCase();
    return message.indexOf('pin is locked') >= 0 ||
        message.indexOf('pin locked') >= 0 ||
        message.indexOf('requires puk') >= 0 ||
        message.indexOf('requires both retry counters') >= 0 ||
        message.indexOf('reset the piv application') >= 0;
}

function _hwIsFactoryDefaultPinError(message) {
    message = String(message || '').toLowerCase();
    return message.indexOf('not at the factory default') >= 0 ||
        message.indexOf('not factory-default') >= 0 ||
        message.indexOf('not factory default') >= 0;
}

function _hwProvisionPayload() {
    return {
        pin: _hwCtx.pin,
        currentPin: _hwCtx.currentPin || null,
        nickname: _hwCtx.nickname,
        force: !!_hwCtx.force
    };
}

function _hwResetPivWithConfirmation(opts) {
    opts = opts || {};
    return new Promise(function(resolve) {
        rsConfirm({
            title: opts.title || 'Reset security key?',
            message: opts.message || 'Ratspeak can reset the YubiKey PIV application and then continue. This erases the Ratspeak identity keys on this YubiKey, but does not affect passkeys, FIDO sign-ins, OTP, or other non-PIV YubiKey features.',
            confirmText: opts.confirmText || 'Reset security key',
            danger: true
        }).then(function(ok) {
            if (!ok) { resolve(false); return; }
            if (typeof opts.beforeReset === 'function') opts.beforeReset();
            RS.invoke('hw_reset_piv').then(function() {
                resolve(true);
            }).catch(function(e) {
                showToast((e && e.message) ? e.message : 'Could not reset the security key', 'toast-error', 7000);
                resolve(false);
            });
        });
    });
}
window.resetHardwarePivWithConfirmation = _hwResetPivWithConfirmation;

function _hwRecoverLockedPinForProvision(msg) {
    _hwShowPinStep();
    var errEl = document.getElementById('hw-pin-error');
    if (errEl) {
        errEl.textContent = msg;
        errEl.style.display = '';
    }
    _hwResetPivWithConfirmation({
        title: 'Reset security key?',
        message: 'The YubiKey PIN is locked. Ratspeak can reset the key’s PIV application and continue setup. This erases any Ratspeak identity currently on this YubiKey, but does not affect passkeys, FIDO sign-ins, OTP, or other non-PIV features.',
        beforeReset: function() { _hwShowWorking('Resetting your security key…'); }
    }).then(function(reset) {
        if (!reset || !_hwCtx) return;
        _hwCtx.currentPin = null;
        _hwDispatchProvision();
    });
}

function _hwRecoverNonFactoryPinForProvision(msg) {
    _hwShowPinStep();
    var errEl = document.getElementById('hw-pin-error');
    if (errEl) {
        errEl.textContent = msg;
        errEl.style.display = '';
    }
    _hwResetPivWithConfirmation({
        title: 'Reset this security key?',
        message: 'This YubiKey is already initialized, so Ratspeak cannot set it up as a new identity until the PIV application is reset. Resetting erases any Ratspeak identity keys on this YubiKey, but does not affect passkeys, FIDO sign-ins, OTP, or other non-PIV features.',
        beforeReset: function() { _hwShowWorking('Resetting your security key…'); }
    }).then(function(reset) {
        if (!reset || !_hwCtx) return;
        _hwCtx.existing = null;
        _hwCtx.currentPin = null;
        _hwCtx.force = false;
        _hwDispatchProvision();
    });
}

function _hwProvisionFailure(err) {
    var msg = (err && err.message) ? err.message : 'Provisioning failed';
    if (_hwIsPinLockedError(msg)) {
        _hwRecoverLockedPinForProvision(msg);
        return;
    }
    if (_hwIsFactoryDefaultPinError(msg)) {
        _hwRecoverNonFactoryPinForProvision(msg);
        return;
    }
    showToast(msg, 'toast-error', 5000);
    _hwShowPinStep();
    var errEl = document.getElementById('hw-pin-error');
    if (errEl) {
        errEl.textContent = msg;
        errEl.style.display = '';
    }
}

function _hwProvisionRecoverable() {
    _hwShowWorking('Provisioning your security key…');
    RS.invoke('hw_provision_recoverable', _hwProvisionPayload())
        .then(function(res) {
            res = res || {};
            _hwCtx.result = res;
            _hwShowMnemonic(res.mnemonic || '');
        })
        .catch(_hwProvisionFailure);
}

function _hwProvisionHardwareOnly() {
    _hwShowWorking('Provisioning your security key…');
    RS.invoke('hw_provision_hardware_only', _hwProvisionPayload())
        .then(function(res) {
            res = res || {};
            _hwFinish(res);
        })
        .catch(_hwProvisionFailure);
}

function _hwShowMnemonic(mnemonic) {
    _hwSetTitle('Write down your recovery phrase');
    var words = recoveryPhraseWords(mnemonic);
    _hwCtx.mnemonic = mnemonic;
    _hwCtx.mnemonicWords = words;

    var grid = document.getElementById('hw-mnemonic-grid');
    if (grid) {
        grid.innerHTML = words.map(function(w) { return '<li>' + escapeHtml(w) + '</li>'; }).join('');
        grid.setAttribute('aria-hidden', 'true');
    }
    var cover = document.getElementById('hw-mnemonic-cover');
    if (cover) cover.hidden = false;
    var shell = document.getElementById('hw-mnemonic-shell');
    if (shell) shell.classList.add('is-covered');

    var confirmChk = document.getElementById('hw-mnemonic-confirm');
    if (confirmChk) confirmChk.checked = false;
    var continueBtn = document.getElementById('hw-mnemonic-continue-btn');
    if (continueBtn) continueBtn.disabled = true;

    _hwShowStep('hw-step-mnemonic');
}

// ---- Restore from phrase ----

function _hwBeginRestore() {
    if (!_hwCtx) _hwCtx = { fromSetup: false };
    _hwCtx.currentPin = null;
    _hwSetTitle('Restore to a hardware key');
    ['hw-restore-phrase', 'hw-restore-nickname', 'hw-restore-pin', 'hw-restore-pin-confirm'].forEach(function(id) {
        var el = document.getElementById(id);
        if (el) el.value = '';
    });
    var err = document.getElementById('hw-restore-error');
    if (err) err.style.display = 'none';
    _hwUpdateRestoreState();
    _hwShowStep('hw-step-restore');
    _hwOpenSheet();
    setTimeout(function() { var p = document.getElementById('hw-restore-phrase'); if (p && !isMobile()) p.focus(); }, 120);
}

function _hwRestoreWordCount() {
    var ta = document.getElementById('hw-restore-phrase');
    var words = String(ta ? ta.value : '').trim().split(/\s+/).filter(Boolean);
    return words.length;
}

function _hwUpdateRestoreState() {
    var count = _hwRestoreWordCount();
    var countEl = document.getElementById('hw-restore-word-count');
    if (countEl) countEl.textContent = count + ' / ' + RECOVERY_PHRASE_WORDS + ' words';
    var pin = document.getElementById('hw-restore-pin').value;
    var confirm = document.getElementById('hw-restore-pin-confirm').value;
    var btn = document.getElementById('hw-restore-btn');
    if (btn) btn.disabled = !(count === RECOVERY_PHRASE_WORDS && _hwPinValid(pin) && pin === confirm);
}

function _hwDoRestore() {
    var phrase = document.getElementById('hw-restore-phrase').value.trim().replace(/\s+/g, ' ');
    var nickname = document.getElementById('hw-restore-nickname').value.trim();
    var pin = document.getElementById('hw-restore-pin').value;
    var confirm = document.getElementById('hw-restore-pin-confirm').value;
    var err = document.getElementById('hw-restore-error');

    var count = phrase ? phrase.split(' ').length : 0;
    if (count !== RECOVERY_PHRASE_WORDS) {
        if (err) { err.textContent = 'Recovery phrase must be exactly ' + RECOVERY_PHRASE_WORDS + ' words.'; err.style.display = ''; }
        return;
    }
    if (!_hwPinValid(pin)) {
        if (err) { err.textContent = 'PIN must be 6–8 characters.'; err.style.display = ''; }
        return;
    }
    if (pin !== confirm) {
        if (err) { err.textContent = "PINs don't match."; err.style.display = ''; }
        return;
    }
    if (err) err.style.display = 'none';
    if (_hwCtx) _hwCtx.pin = pin;

    _hwShowWorking('Restoring identity onto your security key…');
    RS.invoke('hw_restore', {
        phrase: phrase,
        pin: pin,
        currentPin: (_hwCtx && _hwCtx.currentPin) || null,
        nickname: nickname,
        force: false
    })
        .then(function(res) {
            _hwFinish(res || {});
        })
        .catch(function(e) {
            // Keep the typed phrase/name so the user can fix the PIN and retry.
            var msg = (e && e.message) ? e.message : 'Restore failed';
            _hwShowStep('hw-step-restore');
            var errEl = document.getElementById('hw-restore-error');
            if (errEl) { errEl.textContent = msg; errEl.style.display = ''; }
            if (_hwIsPinLockedError(msg)) {
                _hwResetPivWithConfirmation({
                    title: 'Reset security key?',
                    message: 'The YubiKey PIN is locked. Ratspeak can reset the key’s PIV application and then restore this phrase onto it. This erases any Ratspeak identity currently on this YubiKey, but does not affect passkeys, FIDO sign-ins, OTP, or other non-PIV features.',
                    beforeReset: function() { _hwShowWorking('Resetting your security key…'); }
                }).then(function(reset) {
                    if (!reset || !_hwCtx) return;
                    _hwCtx.currentPin = null;
                    _hwDoRestore();
                });
                return;
            }
            showToast(msg, 'toast-error', 5000);
        });
}

// ---- Import existing ----

function _hwDoImport() {
    var nickname = document.getElementById('hw-import-nickname').value.trim();
    var pinInput = document.getElementById('hw-import-pin');
    var pin = pinInput ? pinInput.value : '';
    var err = document.getElementById('hw-import-error');
    if (_hwCtx && _hwCtx.fromSetup && !_hwPinValid(pin)) {
        if (err) { err.textContent = 'Enter the current YubiKey PIN to finish setup.'; err.style.display = ''; }
        return;
    }
    if (_hwCtx && _hwCtx.fromSetup) _hwCtx.pin = pin;
    if (err) err.style.display = 'none';
    _hwShowWorking('Registering your security key…');
    RS.invoke('hw_import_existing', { nickname: nickname })
        .then(function(res) { _hwFinish(res || {}); })
        .catch(function(e) {
            showToast((e && e.message) ? e.message : 'Could not register the security key', 'toast-error', 5000);
            _hwShowImportStep();
        });
}

// Completion: setup path reloads via the import-completion flow; running-app
// path switches to the new identity and closes the sheet.
function _hwFinish(result) {
    // Read context before closing — closeHardwareWizard() nulls _hwCtx.
    var fromSetup = _hwCtx && _hwCtx.fromSetup;
    var pinForSetup = fromSetup && _hwCtx ? _hwCtx.pin : null;
    var newHash = result && result.hash;
    closeHardwareWizard();
    if (fromSetup) {
        if (pinForSetup && newHash && typeof completeSetupAfterHardwareIdentity === 'function') {
            completeSetupAfterHardwareIdentity(result, pinForSetup);
            return;
        }
        // Without the new identity's hash its PIN can't be staged; it opens
        // to the unlock prompt instead.
        if (typeof completeSetupAfterIdentityImport === 'function') {
            completeSetupAfterIdentityImport(result);
        } else {
            reloadToAppLanding();
        }
        return;
    }
    showToast('Hardware identity added', 'toast-success', 3000);
    loadIdentities();
    if (newHash) switchToIdentity(newHash);
}

function _hwCopyMnemonic() {
    var phrase = _hwCtx && _hwCtx.mnemonic;
    if (!phrase) return;
    RS.copySecret(phrase).then(function(ok) {
        if (ok) showSecretCopiedToast('Recovery phrase');
        else showToast('Could not copy the recovery phrase', 'toast-error', 2000);
    });
}

document.addEventListener('DOMContentLoaded', function() {
    var hwClose = document.getElementById('hw-modal-close');
    if (hwClose) hwClose.addEventListener('click', closeHardwareWizard);

    var hwRetry = document.getElementById('hw-detect-retry-btn');
    if (hwRetry) hwRetry.addEventListener('click', _hwRunDetect);

    var modeRecoverable = document.getElementById('hw-mode-recoverable');
    if (modeRecoverable) modeRecoverable.addEventListener('click', function() {
        _hwChooseProvisionMode('recoverable');
    });
    var modeHardwareOnly = document.getElementById('hw-mode-hardware-only');
    if (modeHardwareOnly) modeHardwareOnly.addEventListener('click', function() {
        rsConfirm({
            title: 'Are you sure?',
            message: 'If you do not back up this identity, you will be unable to recover or use it without your YubiKey.',
            confirmText: 'Continue without backup',
            danger: true
        }).then(function(ok) {
            if (!ok || !_hwCtx) return;
            _hwChooseProvisionMode('hardware-only');
        });
    });

    ['hw-detect-back-btn', 'hw-mode-back-btn', 'hw-import-back-btn', 'hw-restore-back-btn'].forEach(function(id) {
        var btn = document.getElementById(id);
        if (btn) btn.addEventListener('click', _hwReturnToChoice);
    });
    var pinBack = document.getElementById('hw-pin-back-btn');
    if (pinBack) pinBack.addEventListener('click', function() {
        if (_hwCtx && _hwCtx.afterDetect === 'provision') _hwShowModeStep();
        else _hwReturnToChoice();
    });

    var pin = document.getElementById('hw-pin');
    var pinConfirm = document.getElementById('hw-pin-confirm');
    if (pin) pin.addEventListener('input', _hwUpdatePinContinue);
    if (pinConfirm) {
        pinConfirm.addEventListener('input', _hwUpdatePinContinue);
        pinConfirm.addEventListener('keydown', function(e) {
            if (e.key === 'Enter') {
                e.preventDefault();
                var b = document.getElementById('hw-pin-continue-btn');
                if (b && !b.disabled) b.click();
            }
        });
    }
    var pinContinue = document.getElementById('hw-pin-continue-btn');
    if (pinContinue) pinContinue.addEventListener('click', _hwPinContinue);

    var revealCover = document.getElementById('hw-mnemonic-cover');
    if (revealCover) revealCover.addEventListener('click', function() {
        revealCover.hidden = true;
        var shell = document.getElementById('hw-mnemonic-shell');
        if (shell) shell.classList.remove('is-covered');
        var grid = document.getElementById('hw-mnemonic-grid');
        if (grid) grid.removeAttribute('aria-hidden');
    });
    var copyBtn = document.getElementById('hw-mnemonic-copy-btn');
    if (copyBtn) copyBtn.addEventListener('click', _hwCopyMnemonic);
    var mnemonicConfirm = document.getElementById('hw-mnemonic-confirm');
    if (mnemonicConfirm) mnemonicConfirm.addEventListener('change', function() {
        var btn = document.getElementById('hw-mnemonic-continue-btn');
        if (btn) btn.disabled = !mnemonicConfirm.checked;
    });
    var mnemonicContinue = document.getElementById('hw-mnemonic-continue-btn');
    if (mnemonicContinue) mnemonicContinue.addEventListener('click', function() {
        if (mnemonicContinue.disabled) return;
        _hwFinish((_hwCtx && _hwCtx.result) || {});
    });

    var restorePhrase = document.getElementById('hw-restore-phrase');
    if (restorePhrase) restorePhrase.addEventListener('input', _hwUpdateRestoreState);
    ['hw-restore-pin', 'hw-restore-pin-confirm'].forEach(function(id) {
        var el = document.getElementById(id);
        if (el) el.addEventListener('input', _hwUpdateRestoreState);
    });
    var restoreBtn = document.getElementById('hw-restore-btn');
    if (restoreBtn) restoreBtn.addEventListener('click', _hwDoRestore);

    var importBtn = document.getElementById('hw-import-btn');
    if (importBtn) importBtn.addEventListener('click', _hwDoImport);
    var importNick = document.getElementById('hw-import-nickname');
    if (importNick) importNick.addEventListener('keydown', function(e) {
        if (e.key === 'Enter') {
            e.preventDefault();
            _hwDoImport();
        }
    });
    var importPin = document.getElementById('hw-import-pin');
    if (importPin) {
        importPin.addEventListener('input', function() {
            var b = document.getElementById('hw-import-btn');
            if (b && _hwCtx && _hwCtx.fromSetup) b.disabled = !_hwPinValid(importPin.value);
        });
        importPin.addEventListener('keydown', function(e) {
            if (e.key === 'Enter') {
                e.preventDefault();
                var b = document.getElementById('hw-import-btn');
                if (!b || !b.disabled) _hwDoImport();
            }
        });
    }

    var hwOverlay = document.getElementById('hw-modal-overlay');
    if (hwOverlay) hwOverlay.addEventListener('click', function(e) {
        if (e.target === hwOverlay) closeHardwareWizard();
    });

    document.addEventListener('keydown', function(e) {
        if (e.key === 'Escape') {
            var modal = document.getElementById('hw-modal');
            if (modal && modal.classList.contains('open')) closeHardwareWizard();
        }
    });
});

// Settings-tab utility: detect + import an existing hardware key directly.
function removeHardwareIdentity(target) {
    if (!target || !target.hash) return;
    rsConfirm({
        title: 'Remove Hardware Identity',
        message: 'This removes the hardware identity "' + identityDisplayName(target) + '" from this device only. The security key itself is not modified — its keys stay on the device, and you can add it again later.',
        confirmText: 'Remove',
        danger: true
    }).then(function(ok) {
        if (!ok) return;
        var removePromise;
        if (target.is_active) {
            var firstRemaining = null;
            for (var j = 0; j < identityList.length; j++) {
                if (identityList[j].hash !== target.hash) { firstRemaining = identityList[j]; break; }
            }
            if (!firstRemaining) {
                showPreConditionToast('No other identity to switch to');
                return;
            }
            removePromise = RS.invoke('api_activate_identity', { hashHex: firstRemaining.hash })
                .then(function() {
                    activeIdentityHash = firstRemaining.hash;
                    return RS.invoke('hw_remove', { hash: target.hash });
                });
        } else {
            removePromise = RS.invoke('hw_remove', { hash: target.hash });
        }
        removePromise.then(function() {
            showToast('Hardware identity removed', 'toast-success', 3000);
            selectedIdentityHash = null;
            loadIdentities();
        }).catch(function(err) {
            showToast((err && err.message) ? err.message : 'Could not remove the identity', 'toast-error', 3000);
        });
    });
}

// ---- Protected identity unlock (PIN prompt) ----
// Shown when the active identity is hardware-backed or software PIN-protected
// (on boot, or after the auto-lock timeout). Unlocking re-inits the runtime.

var _hwLockedHash = null;
var _hwLockedKind = 'hardware'; // 'hardware' (YubiKey PIN) | 'passcode' (software)

// Launch/timeout unlock prompt. Handles both a hardware-key PIN and a software
// passcode (kind) through the generic identity unlock command.
function showHwUnlock(hash, kind) {
    if (typeof hash === 'string' && hash) _hwLockedHash = hash;
    if (kind === 'passcode' || kind === 'hardware') _hwLockedKind = kind;
    var isPass = _hwLockedKind === 'passcode';
    var existing = document.getElementById('hw-unlock-overlay');
    if (existing) existing.remove();
    var overlay = document.createElement('div');
    overlay.id = 'hw-unlock-overlay';
    overlay.className = 'hw-unlock-overlay';
    overlay.style.display = 'flex';
    var title = isPass ? 'Unlock your identity' : 'Unlock your hardware key';
    var sub = isPass
        ? 'Enter your PIN to continue.'
        : 'Enter your YubiKey PIN to continue. Keep the key plugged in.';
    var inputAttrs = isPass
        ? 'type="password" autocomplete="off" maxlength="128" placeholder="PIN"'
        : 'type="password" inputmode="numeric" autocomplete="off" maxlength="8" placeholder="PIN"';
    overlay.innerHTML =
        '<div class="hw-unlock-card">' +
            '<div class="hw-unlock-icon">' + HW_BADGE_ICON + '</div>' +
            '<div class="hw-unlock-title">' + title + '</div>' +
            '<div class="hw-unlock-sub">' + sub + '</div>' +
            '<input id="hw-unlock-pin" class="hw-unlock-input" ' + inputAttrs + '>' +
            '<div id="hw-unlock-error" class="hw-unlock-error" style="display:none"></div>' +
            '<button id="hw-unlock-btn" class="hw-unlock-btn" disabled>Unlock</button>' +
            '<button id="hw-unlock-reset" class="hw-unlock-btn hw-unlock-reset" style="display:none;">Reset security key</button>' +
            '<button id="hw-unlock-cancel" class="hw-unlock-cancel">Use a different identity</button>' +
            '<button id="hw-unlock-forget" class="hw-unlock-cancel hw-unlock-forget" style="display:none;">Remove this identity from this device</button>' +
        '</div>';
    document.body.appendChild(overlay);
    var input = overlay.querySelector('#hw-unlock-pin');
    var btn = overlay.querySelector('#hw-unlock-btn');
    var validLen = function(v) { return isPass ? v.length >= 6 : (v.length >= 6 && v.length <= 8); };
    input.addEventListener('input', function() { btn.disabled = !validLen(input.value); });
    input.addEventListener('keydown', function(e) {
        if (e.key === 'Enter' && !btn.disabled) _hwDoUnlock();
    });
    btn.addEventListener('click', _hwDoUnlock);
    overlay.querySelector('#hw-unlock-reset').addEventListener('click', _hwResetLockedIdentity);
    overlay.querySelector('#hw-unlock-cancel').addEventListener('click', _hwUnlockCancel);
    overlay.querySelector('#hw-unlock-forget').addEventListener('click', _hwForgetLockedIdentity);
    setTimeout(function() { if (!isMobile()) input.focus(); }, 120);
}
window.showHwUnlock = showHwUnlock;

function _hwDoUnlock() {
    var input = document.getElementById('hw-unlock-pin');
    var btn = document.getElementById('hw-unlock-btn');
    var err = document.getElementById('hw-unlock-error');
    var resetBtn = document.getElementById('hw-unlock-reset');
    var secret = input ? input.value : '';
    var isPass = _hwLockedKind === 'passcode';
    if (secret.length < 6 || (!isPass && secret.length > 8)) return;
    btn.disabled = true;
    btn.textContent = 'Unlocking…';
    if (resetBtn) resetBtn.style.display = 'none';
    if (err) err.style.display = 'none';
    RS.invoke('unlock_identity', { secret: secret }).then(function(res) {
        res = res || {};
        if (res.ok) {
            // Re-bootstrap the whole app on the now-unlocked identity.
            window.location.reload();
            return;
        }
        btn.disabled = false;
        btn.textContent = 'Unlock';
        if (input) input.value = '';
        var msg;
        if (isPass) {
            msg = 'Incorrect PIN.';
        } else if (res.locked) {
            msg = 'This key is locked after too many wrong PINs. Reset the security key to use it again.';
            btn.disabled = true;
            if (resetBtn) resetBtn.style.display = '';
        } else if (typeof res.remaining === 'number') {
            msg = 'Incorrect PIN — ' + res.remaining + ' attempt' + (res.remaining === 1 ? '' : 's') + ' left.';
        } else {
            msg = res.error || 'Could not unlock. Is the key plugged in?';
        }
        if (err) { err.textContent = msg; err.style.display = ''; }
    }).catch(function(e) {
        btn.disabled = false;
        btn.textContent = 'Unlock';
        if (err) { err.textContent = (e && e.message) || 'Unlock failed.'; err.style.display = ''; }
    });
}

function _hwResetLockedIdentity() {
    if (_hwLockedKind !== 'hardware') return;
    var err = document.getElementById('hw-unlock-error');
    _hwResetPivWithConfirmation({
        title: 'Reset security key?',
        message: 'This erases the Ratspeak identity keys on this YubiKey and removes the local registration from this device. Use this only if you have a recovery phrase or are willing to create a new identity. Passkeys, FIDO sign-ins, OTP, and other non-PIV features are not affected.',
        beforeReset: function() {
            var btn = document.getElementById('hw-unlock-reset');
            if (btn) {
                btn.disabled = true;
                btn.textContent = 'Resetting...';
            }
        }
    }).then(function(reset) {
        if (!reset) {
            var btn = document.getElementById('hw-unlock-reset');
            if (btn) {
                btn.disabled = false;
                btn.textContent = 'Reset security key';
            }
            return;
        }
        if (!_hwLockedHash) {
            window.location.href = '/';
            window.location.reload();
            return;
        }
        RS.invoke('hw_remove', { hash: _hwLockedHash }).then(function() {
            try {
                localStorage.removeItem('ratspeak_identity_hash');
                localStorage.removeItem('ratspeak_identity_name');
            } catch (_) {}
            window.location.href = '/';
            window.location.reload();
        }).catch(function(e) {
            if (err) {
                err.textContent = (e && e.message) ? e.message : 'Security key reset, but local identity removal failed.';
                err.style.display = '';
            }
        });
    });
}

// Escape hatch: switch to another identity (e.g. a key that can't be unlocked
// because it was re-provisioned, or the wrong key is plugged in).
function _hwUnlockCancel() {
    var err = document.getElementById('hw-unlock-error');
    RS.invoke('api_list_identities').then(function(list) {
        list = list || [];
        var other = null;
        for (var i = 0; i < list.length; i++) {
            if (list[i] && list[i].hash !== _hwLockedHash) { other = list[i]; break; }
        }
        if (!other) {
            if (err) {
                err.textContent = 'This is your only identity on this device. You can remove the local registration and return to setup; the YubiKey itself will not be erased.';
                err.style.display = '';
            }
            var forget = document.getElementById('hw-unlock-forget');
            if (forget && _hwLockedKind === 'hardware' && _hwLockedHash) forget.style.display = '';
            return;
        }
        RS.invoke('switch_identity', { hash: other.hash }).finally(function() {
            window.location.reload();
        });
    }).catch(function() { window.location.reload(); });
}

function _hwForgetLockedIdentity() {
    if (!_hwLockedHash || _hwLockedKind !== 'hardware') return;
    var err = document.getElementById('hw-unlock-error');
    rsConfirm({
        title: 'Remove hardware identity?',
        message: 'This removes the locked hardware identity from this device and returns to setup. It does not erase or reset the YubiKey; you can add it again later with the key\'s current PIN.',
        confirmText: 'Remove from this device',
        danger: true
    }).then(function(ok) {
        if (!ok) return;
        var btn = document.getElementById('hw-unlock-forget');
        if (btn) {
            btn.disabled = true;
            btn.textContent = 'Removing...';
        }
        RS.invoke('hw_remove', { hash: _hwLockedHash }).then(function() {
            try {
                localStorage.removeItem('ratspeak_identity_hash');
                localStorage.removeItem('ratspeak_identity_name');
            } catch (_) {}
            window.location.href = '/';
            window.location.reload();
        }).catch(function(e) {
            if (btn) {
                btn.disabled = false;
                btn.textContent = 'Remove this identity from this device';
            }
            if (err) {
                err.textContent = (e && e.message) ? e.message : 'Failed to remove the local hardware identity.';
                err.style.display = '';
            }
        });
    });
}

if (typeof RS !== 'undefined' && RS.listen) {
    RS.listen('hardware_locked', function(data) {
        // An auto-lock starts the page over, so none of the identity's data
        // stays behind the PIN sheet.
        if (data && data.reason === 'timeout') {
            window.location.reload();
            return;
        }
        showHwUnlock(data && data.hash, data && data.kind);
    });
}
