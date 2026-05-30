var identityList = [];
var selectedIdentityHash = null;
var activeIdentityHash = null;
var RECOVERY_PHRASE_WORDS = 12;

function shareAddress(address, displayName) {
    if (!navigator.share) {
        if (navigator.clipboard) {
            navigator.clipboard.writeText(address).then(function() {
                showCopyConfirmationToast('Address');
            });
        }
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

function copyIdentityValue(value, noun) {
    if (!value) return;
    if (!navigator.clipboard) {
        shareAddress(value, '');
        return;
    }
    navigator.clipboard.writeText(value).then(function() {
        showCopyConfirmationToast(noun || 'Value');
    }).catch(function() {});
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

function identityPasscodeOptionHtml(prefix, opts) {
    opts = opts || {};
    var label = opts.label || 'Encrypt this identity on this device';
    var help = opts.help || 'Require a passcode when Ratspeak opens. Keep your ' +
        RECOVERY_PHRASE_WORDS + '-word phrase; forgotten passcodes cannot be recovered.';
    return '' +
        '<label class="rs-dialog-checkbox-wrap identity-passcode-option">' +
            '<input type="checkbox" id="' + prefix + '-passcode-enable" class="rs-dialog-checkbox">' +
            '<span class="rs-dialog-checkbox-label">' + escapeHtml(label) + '</span>' +
            '<span class="rs-dialog-checkbox-help">' + escapeHtml(help) + '</span>' +
        '</label>' +
        '<div class="identity-passcode-fields" id="' + prefix + '-passcode-fields" hidden>' +
            '<div class="modal-field">' +
                '<label>Passcode</label>' +
                '<input type="password" id="' + prefix + '-passcode-new" class="modal-input" maxlength="128" autocomplete="off" placeholder="At least 6 characters">' +
            '</div>' +
            '<div class="modal-field">' +
                '<label>Confirm Passcode</label>' +
                '<input type="password" id="' + prefix + '-passcode-confirm" class="modal-input" maxlength="128" autocomplete="off">' +
            '</div>' +
        '</div>' +
        '<div class="modal-error" id="' + prefix + '-passcode-error" style="display:none;"></div>';
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
        identitySetInlineError(errId, 'Passcode must be at least 6 characters.');
        return null;
    }
    if (passcode !== confirm) {
        identitySetInlineError(errId, "Passcodes don't match.");
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
}

function identityImportFormatChoices() {
    return [
        {
            label: 'Ratspeak Identity Backup',
            value: 'ratspeak',
            hint: 'Import a .rsi identity backup created by Ratspeak.'
        },
        {
            label: 'Reticulum Identity Key',
            value: 'reticulum',
            hint: 'Import a raw, base32, base64, or hex Reticulum private identity key.'
        },
        {
            label: 'Recovery Phrase',
            value: 'phrase',
            hint: 'Restore from a 12-word recovery phrase (creates a software identity).'
        }
    ];
}

function identityExportFormatChoices() {
    return [
        {
            label: 'Ratspeak Identity Backup',
            value: 'ratspeak',
            hint: 'Advanced: unencrypted .rsi private-key backup with display-name metadata.'
        },
        {
            label: 'Reticulum Identity File',
            value: 'reticulum',
            hint: 'Advanced: raw unencrypted 64-byte Reticulum private identity key.'
        },
        {
            label: 'Reticulum Base32 Key',
            value: 'reticulum-base32',
            hint: 'Advanced: unencrypted text form of the same private identity key.'
        }
    ];
}

function chooseIdentityImportFormat() {
    return rsChoice({
        title: 'Import Identity',
        message: 'Choose the source format.',
        choices: identityImportFormatChoices()
    });
}

function chooseIdentityExportFormat() {
    return rsChoice({
        title: 'Export Identity',
        message: 'Recovery phrases are preferred for normal backup. These exports create unencrypted private-key material.',
        choices: identityExportFormatChoices()
    });
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

function saveBytesToUserFile(bytes, fileName, mimeType, backupBase64) {
    if (hasAndroidIdentityDocumentBridge()) {
        return saveIdentityDocumentWithAndroid(fileName, backupBase64 || bytesToBase64(bytes), mimeType);
    }
    if (hasAndroidIdentityExportBridge() && /\.rsi$/i.test(fileName || '')) {
        return saveIdentityBackupWithAndroid(fileName, backupBase64 || bytesToBase64(bytes));
    }

    var blob = new Blob([bytes], { type: mimeType || 'application/octet-stream' });
    var url = URL.createObjectURL(blob);
    var a = document.createElement('a');
    a.href = url;
    a.download = fileName;
    a.style.display = 'none';
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(function() { try { URL.revokeObjectURL(url); } catch (_) {} }, 60000);
    return Promise.resolve({ method: 'download', fileName: fileName });
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

// Blockies-style SVG identicon. Adapted from https://github.com/download13/blockies (MIT).
var blockies = (function() {
    function seedRand(seed) {
        var s = [0, 0, 0, 0];
        for (var i = 0; i < seed.length; i++) {
            s[i % 4] = (s[i % 4] << 5) - s[i % 4] + seed.charCodeAt(i);
        }
        return function() {
            var t = s[0] ^ (s[0] << 11);
            s[0] = s[1]; s[1] = s[2]; s[2] = s[3];
            s[3] = (s[3] ^ (s[3] >> 19) ^ t ^ (t >> 8)) >>> 0;
            return s[3] / ((1 << 31) >>> 0);
        };
    }

    function createColor(rand) {
        var h = Math.floor(rand() * 360);
        var s = ((rand() * 60) + 40);
        var l = ((rand() + rand() + rand() + rand()) * 25);
        return 'hsl(' + h + ',' + s + '%,' + l + '%)';
    }

    function createImageData(rand, gridSize) {
        var w = gridSize, h = gridSize;
        var halfW = Math.ceil(w / 2);
        var data = [];
        for (var y = 0; y < h; y++) {
            var row = [];
            for (var x = 0; x < halfW; x++) {
                // 0 = bg, 1 = primary, 2 = spot
                row.push(Math.floor(rand() * 2.3));
            }
            var fullRow = row.slice();
            for (var x = Math.floor(w / 2) - 1; x >= 0; x--) {
                fullRow.push(row[x]);
            }
            data.push(fullRow);
        }
        return data;
    }

    var fn = function(seed, svgSize) {
        var gridSize = 8;
        var rand = seedRand(seed || '');
        var color = createColor(rand);
        var bgcolor = createColor(rand);
        var spotcolor = createColor(rand);
        var grid = createImageData(rand, gridSize);

        var rects = '';
        for (var y = 0; y < gridSize; y++) {
            for (var x = 0; x < gridSize; x++) {
                var val = grid[y][x];
                var fill = val === 0 ? bgcolor : val === 1 ? color : spotcolor;
                rects += '<rect x="' + x + '" y="' + y + '" width="1" height="1" fill="' + fill + '"/>';
            }
        }

        return '<svg xmlns="http://www.w3.org/2000/svg" width="' + svgSize +
            '" height="' + svgSize + '" viewBox="0 0 ' + gridSize + ' ' + gridSize +
            '" shape-rendering="crispEdges" style="display:block;border-radius:50%;clip-path:circle(50% at 50% 50%);overflow:hidden;">' +
            rects + '</svg>';
    };
    return fn;
})();

// Cache avatars per (hash, size) — blockies PRNG + 64 SVG rects is expensive per call.
var _avatarCache = {};
function identityAvatar(hashValue, size) {
    if (!hashValue) {
        var color = 'var(--text-muted)';
        var radius = size / 2;
        return '<svg width="' + size + '" height="' + size + '" viewBox="0 0 ' + size + ' ' + size +
            '" style="display:block;border-radius:50%;clip-path:circle(50% at 50% 50%);overflow:hidden;">' +
            '<circle cx="' + radius + '" cy="' + radius + '" r="' + radius + '" fill="' + color + '" opacity="0.3"/>' +
            '</svg>';
    }
    var key = hashValue + '|' + size;
    if (_avatarCache[key]) return _avatarCache[key];
    var svg = blockies(hashValue, size);
    _avatarCache[key] = svg;
    return svg;
}

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
        var titleEl = document.getElementById('identity-page-title');
        if (titleEl) {
            titleEl.textContent = _activeIdent ? 'Identity Management' : 'No identity loaded';
        }
        // Per-section try/catch — one render failure shouldn't block others.
        try { renderActiveIdentityCard(); }
        catch(e) { window.RS.diag('error', '[Identity] Active card render error:', e); }

        try { renderIdentityList(); }
        catch(e) { window.RS.diag('error', '[Identity] List render error:', e); }

        try { renderNetworkIdentityCard(); }
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

function renderActiveIdentityCard() {
    var container = document.getElementById('identity-active-card');
    if (!container) return;

    var identity = selectedIdentity();

    if (!identity) {
        container.innerHTML = '<div class="text-muted-color text-sm">No active identity.</div>';
        return;
    }

    var nickname = escapeHtml(identityDisplayName(identity));
    var displayName = identity.display_name || '';
    var lxmfHash = identity.lxmf_hash || '';
    var identityHash = identity.hash || '';
    var isActive = !!identity.is_active;
    var isHardware = !!identity.is_hardware;
    var isOriginal = isOriginalIdentity(identityHash);
    var canDelete = !isOriginal && (!isActive || identityList.length > 1);
    var activeLabel = isActive ? 'Active' : 'Stored';
    var deleteTitle = isOriginal ? 'The original identity cannot be deleted' :
        (identityList.length <= 1 ? 'The only identity cannot be deleted' : 'Delete identity');

    var avatarHtml = identityAvatar(lxmfHash || identityHash, 72);
    var switchAction = isActive ? '' :
        '<button class="identity-action-row identity-action-row--primary" id="identity-switch-detail-btn">' +
            '<span class="identity-action-icon"><svg viewBox="0 0 24 24"><path d="M16 3h5v5"/><path d="M4 20l17-17"/><path d="M21 16v5h-5"/><path d="M15 15l6 6"/><path d="M4 4l5 5"/></svg></span>' +
            '<span>Switch to Identity</span>' +
        '</button>';
    var editorHtml = isActive ?
        '<div class="identity-detail-editor">' +
            '<div class="modal-field">' +
                '<label>Display Name</label>' +
                '<div class="settings-display-name-row">' +
                    '<input type="text" id="identity-display-name" class="modal-input" placeholder="Optional" maxlength="32" value="' + escapeHtml(displayName) + '">' +
                    '<button class="nr-btn" id="identity-save-name-btn" style="display:none;">Save</button>' +
                '</div>' +
            '</div>' +
        '</div>' : '';

    container.innerHTML =
        '<div class="identity-detail-hero">' +
            '<div class="identity-avatar identity-detail-avatar">' + avatarHtml + '</div>' +
            '<div class="identity-detail-heading">' +
                '<div class="identity-card-nickname">' + nickname + '</div>' +
                '<div class="identity-status-row">' +
                    '<span class="identity-active-badge">' + activeLabel + '</span>' +
                    (isHardware ? '<span class="identity-hardware-badge" title="' + (identity.hw_serial ? 'YubiKey serial ' + escapeHtml(String(identity.hw_serial)) : 'Hardware key identity') + '">' + HW_BADGE_ICON + 'Hardware Key' + (identity.hw_serial ? ' · ' + escapeHtml(String(identity.hw_serial)) : '') + '</span>' : '<span class="identity-private-badge">Private Key</span>') +
                    (isOriginal ? '<span class="identity-private-badge">Original</span>' : '') +
                '</div>' +
            '</div>' +
        '</div>' +
        '<div class="identity-address-stack">' +
            '<button type="button" class="identity-address-row" data-copy-value="' + escapeHtml(lxmfHash) + '" data-copy-label="Address">' +
                '<span class="identity-address-meta">' +
                    '<span class="identity-label">LXMF Address</span>' +
                    '<span class="identity-value mono">' + copyableHash(lxmfHash) + '</span>' +
                '</span>' +
                '<span class="identity-address-action"><svg viewBox="0 0 24 24"><rect x="9" y="9" width="13" height="13" rx="2"/><rect x="2" y="2" width="13" height="13" rx="2"/></svg></span>' +
            '</button>' +
            '<button type="button" class="identity-address-row" data-copy-value="' + escapeHtml(identityHash) + '" data-copy-label="Hash">' +
                '<span class="identity-address-meta">' +
                    '<span class="identity-label">Identity Hash</span>' +
                    '<span class="identity-value mono">' + copyableHash(identityHash) + '</span>' +
                '</span>' +
                '<span class="identity-address-action"><svg viewBox="0 0 24 24"><rect x="9" y="9" width="13" height="13" rx="2"/><rect x="2" y="2" width="13" height="13" rx="2"/></svg></span>' +
            '</button>' +
        '</div>' +
        editorHtml +
        '<div class="identity-detail-actions">' +
            switchAction +
            (isHardware ? '' :
            '<button class="identity-action-row" id="identity-export-detail-btn">' +
                '<span class="identity-action-icon"><svg viewBox="0 0 24 24"><path d="M12 21V9"/><path d="M7 14l5-5 5 5"/><path d="M5 21h14"/></svg></span>' +
                '<span>Export Identity</span>' +
            '</button>') +
            (isHardware || !identity.has_mnemonic ? '' :
            '<button class="identity-action-row" id="identity-view-phrase-btn">' +
                '<span class="identity-action-icon"><svg viewBox="0 0 24 24"><path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7-10-7-10-7z"/><circle cx="12" cy="12" r="3"/></svg></span>' +
                '<span>View Recovery Phrase</span>' +
            '</button>') +
            '<button class="identity-action-row" id="identity-share-address-btn">' +
                '<span class="identity-action-icon"><svg viewBox="0 0 24 24"><rect x="3" y="3" width="7" height="7"/><rect x="14" y="3" width="7" height="7"/><rect x="3" y="14" width="7" height="7"/><path d="M14 14h3v3h-3z"/><path d="M19 14h2"/><path d="M14 21h7v-2"/><path d="M19 17h2"/></svg></span>' +
                '<span>Share Contact Card</span>' +
            '</button>' +
            '<button class="identity-action-row identity-action-row--danger" id="identity-delete-btn"' + (canDelete ? '' : ' disabled aria-disabled="true"') + ' title="' + escapeHtml(deleteTitle) + '">' +
                '<span class="identity-action-icon"><svg viewBox="0 0 24 24"><path d="M3 6h18"/><path d="M8 6V4h8v2"/><path d="M19 6l-1 14H6L5 6"/></svg></span>' +
                '<span>' + (isHardware ? 'Remove Identity' : 'Delete Identity') + '</span>' +
            '</button>' +
        '</div>';

    var copyRows = container.querySelectorAll('[data-copy-value]');
    Array.prototype.forEach.call(copyRows, function(row) {
        row.addEventListener('click', function() {
            copyIdentityValue(row.getAttribute('data-copy-value'), row.getAttribute('data-copy-label'));
        });
    });

    var switchBtn = document.getElementById('identity-switch-detail-btn');
    if (switchBtn) switchBtn.addEventListener('click', function() { switchToIdentity(identityHash); });

    var exportBtn = document.getElementById('identity-export-detail-btn');
    if (exportBtn) exportBtn.addEventListener('click', function() { exportIdentityBackup(identityHash); });

    var viewPhraseBtn = document.getElementById('identity-view-phrase-btn');
    if (viewPhraseBtn) viewPhraseBtn.addEventListener('click', function() { viewRecoveryPhrase(identity); });

    var shareBtn = document.getElementById('identity-share-address-btn');
    if (shareBtn) shareBtn.addEventListener('click', function() {
        if (typeof openIdentityShareScreen === 'function') {
            openIdentityShareScreen(identityHash);
        } else {
            shareAddress(lxmfHash || identityHash, identityDisplayName(identity));
        }
    });

    var deleteBtn = document.getElementById('identity-delete-btn');
    if (deleteBtn) deleteBtn.addEventListener('click', function() {
        if (!deleteBtn.disabled) deleteIdentityByHash(identityHash);
    });

    var nameInput = document.getElementById('identity-display-name');
    var saveBtn = document.getElementById('identity-save-name-btn');
    if (nameInput && saveBtn) {
        nameInput.addEventListener('input', function() {
            var current = nameInput.value.trim();
            saveBtn.style.display = (current !== displayName) ? '' : 'none';
        });
        nameInput.addEventListener('keydown', function(e) {
            if (e.key === 'Enter') {
                e.preventDefault();
                if (saveBtn.style.display !== 'none' && !saveBtn.disabled) saveBtn.click();
            }
        });
        saveBtn.addEventListener('click', function() {
            var newName = nameInput.value.trim();
            saveBtn.disabled = true;
            saveBtn.textContent = 'Saving...';
            RS.invoke('api_set_display_name', { args: { display_name: newName } }).then(function() {
                showToast('Display name saved and announced', 'toast-green', 3000);
                saveBtn.textContent = 'Saved!';
                saveBtn.className = 'nr-btn nr-btn-success';
                setTimeout(function() {
                    saveBtn.style.display = 'none';
                    saveBtn.textContent = 'Save';
                    saveBtn.className = 'nr-btn';
                    saveBtn.disabled = false;
                }, 1500);
                loadIdentities();
            }).catch(function(err) {
                saveBtn.textContent = 'Save';
                saveBtn.disabled = false;
                showToast((err && err.message) ? err.message : 'Failed to save', 'toast-red', 3000);
            });
        });
    }

}

function renderIdentityList() {
    var container = document.getElementById('identity-list');
    if (!container) return;

    if (identityList.length === 0) {
        container.innerHTML = '<div class="inline-hint" style="padding:12px;">No identities found.</div>';
        return;
    }

    // Sort by creation time: original identity stays at position 0.
    var sorted = sortedIdentities();

    container.innerHTML = '';
    sorted.forEach(function(ident, index) {
        var item = document.createElement('div');
        item.className = 'identity-list-item';
        if (ident.hash === selectedIdentityHash) item.classList.add('selected');
        if (ident.is_active) item.classList.add('active-identity');

        var nickname = escapeHtml(identityDisplayName(ident));
        var lxmfHash = ident.lxmf_hash || '';
        var isOriginal = (index === 0);

        var badgeHtml = '';
        if (isOriginal) {
            badgeHtml += '<span class="identity-private-badge identity-private-badge--list">Original</span>';
        }
        if (ident.is_hardware) {
            var hwTitle = ident.hw_serial ? ('Hardware key · YubiKey serial ' + ident.hw_serial) : 'Hardware key identity';
            badgeHtml += '<span class="identity-hardware-badge identity-hardware-badge--list" title="' + escapeHtml(hwTitle) + '">' + HW_BADGE_ICON + 'HW</span>';
        }
        if (ident.is_active) {
            badgeHtml += '<span class="identity-active-badge">Active</span>';
        } else {
            badgeHtml += '<button type="button" class="identity-select-btn" data-hash="' + escapeHtml(ident.hash) + '">Switch</button>';
        }

        item.innerHTML =
            '<div class="identity-list-avatar">' + identityAvatar(ident.lxmf_hash || ident.hash || '', 32) + '</div>' +
            '<div class="identity-list-info">' +
                '<span class="identity-list-name">' + nickname + '</span>' +
                '<span class="identity-list-hash mono">' + escapeHtml(lxmfHash) + '</span>' +
            '</div>' +
            '<div class="identity-list-badges">' + badgeHtml + '</div>';

        item.addEventListener('click', function(e) {
            if (e.target.closest && e.target.closest('.identity-select-btn')) return;
            selectedIdentityHash = ident.hash;
            renderIdentityList();
            renderActiveIdentityCard();
            if (typeof isMobile === 'function' && isMobile()) {
                openIdentityActions(ident.hash);
            }
        });

        container.appendChild(item);
    });

    if (!container._selectDelegated) {
        container._selectDelegated = true;
        container.addEventListener('click', function(e) {
            var btn = e.target.closest ? e.target.closest('.identity-select-btn') : null;
            if (btn) {
                var hash = btn.getAttribute('data-hash');
                if (hash) switchToIdentity(hash);
            }
        });
    }

    updateRemoveButtonState();
}

function updateRemoveButtonState() {
    var removeBtn = document.getElementById('identity-remove-btn');
    if (!removeBtn) return;

    var sorted = identityList.slice().sort(function(a, b) {
        return (a.created_at || 0) - (b.created_at || 0);
    });
    var originalHash = sorted.length > 0 ? sorted[0].hash : null;

    var isOriginal = selectedIdentityHash && selectedIdentityHash === originalHash;
    var isActive = selectedIdentityHash && selectedIdentityHash === activeIdentityHash;

    if (!selectedIdentityHash || isOriginal || isActive) {
        removeBtn.disabled = true;
        if (isOriginal) removeBtn.title = 'The original identity cannot be removed';
        else if (isActive) removeBtn.title = 'The active identity cannot be removed';
        else removeBtn.title = 'Select an identity to remove';
    } else {
        removeBtn.disabled = false;
        removeBtn.title = 'Remove selected identity';
    }
}

function switchToIdentity(hash) {
    var card = document.getElementById('identity-active-card');
    if (card) {
        card.innerHTML = '<div class="identity-switching-overlay"><div class="identity-switching-text">Switching identity...</div></div>';
    }
    // Backend reloads LXMF manager + emits identity_switched, which drives
    // full state cleanup + re-emits initial state for the new identity.
    RS.invoke('switch_identity', { hash: hash }).catch(function() {});
}

function openIdentitySwitcher() {
    if (!identityList || identityList.length <= 1) return;
    var choices = identityList.slice().sort(function(a, b) {
        return (a.created_at || 0) - (b.created_at || 0);
    }).map(function(ident) {
        var name = ident.display_name || ident.nickname || 'Unnamed';
        var hash = ident.lxmf_hash || '';
        var shortLabel = hash ? (typeof shortHash === 'function' ? shortHash(hash, 8, 4) : hash.substring(0, 12) + '\u2026') : '';
        return {
            label: name + (shortLabel ? '  ' + shortLabel : ''),
            value: ident.hash,
            hint: ident.is_active ? 'Currently active' : null
        };
    });
    rsChoice({ title: 'Switch Identity', choices: choices }).then(function(hash) {
        if (!hash || hash === activeIdentityHash) return;
        switchToIdentity(hash);
    });
}

function createNewIdentity() {
    showIdentityModal('Create New Identity',
        '<div class="modal-field">' +
            '<label>Display Name</label>' +
            '<input type="text" id="identity-modal-nickname" class="modal-input" placeholder="e.g. Rat King" maxlength="32">' +
        '</div>' +
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
                    var msg = (err && err.message) ? err.message : 'Could not encrypt identity with passcode';
                    showToast('Identity created, but passcode setup failed: ' + msg, 'toast-red', 7000);
                }) : Promise.resolve();
                var mnemonic = data && data.mnemonic;
                return protect.then(function() {
                    if (mnemonic) {
                        showRecoveryPhraseBackup(mnemonic, function() {
                            showToast('Identity created', 'toast-green', 3000);
                            loadIdentities();
                        }, { passcodeProtected: !!passcode });
                    } else {
                        showToast('Identity created', 'toast-green', 3000);
                        loadIdentities();
                    }
                });
            }).catch(function(err) {
                showToast(err && err.message ? err.message : 'Failed to create identity', 'toast-red', 3000);
                loadIdentities();
            });
        }
    );
    bindIdentityPasscodeOption('identity-create');
}

// Recovery-phrase reveal as a standalone full-screen overlay (reuses the same
// reveal/copy/confirm pattern as the setup hardware-key backup step). Used both
// at software creation/restore and later via "View Recovery Phrase"; re-display
// reads the plaintext sidecar or decrypts it from the passcode vault.
function showRecoveryPhraseBackup(mnemonic, onDone, opts) {
    opts = opts || {};
    var words = String(mnemonic || '').trim().split(/\s+/).filter(Boolean);
    if (words.length !== RECOVERY_PHRASE_WORDS) {
        if (typeof onDone === 'function') onDone();
        return;
    }
    var grid = words.map(function(w, i) {
        return '<div class="hw-mnemonic-word"><span class="hw-mnemonic-index">' + (i + 1) +
            '</span><span class="hw-mnemonic-text">' + escapeHtml(w) + '</span></div>';
    }).join('');
    var storageWarn = opts.passcodeProtected
        ? 'Ratspeak encrypts this identity and phrase with your passcode on this device.'
        : 'Ratspeak stores this phrase on this device so you can view it again; set a passcode to encrypt it at rest.';
    var defaultWarn = 'Write down these ' + RECOVERY_PHRASE_WORDS + ' words in order and store them somewhere safe. ' +
        'Anyone with them controls your identity. ' + storageWarn;
    var requireConfirm = opts.requireConfirm !== false;
    var confirmHtml = requireConfirm
        ? '<label class="hw-confirm-check">' +
              '<input type="checkbox" id="recovery-backup-confirm">' +
              '<span>I have written down my ' + RECOVERY_PHRASE_WORDS + '-word phrase and stored it safely.</span>' +
          '</label>'
        : '';
    var doneLabel = opts.button || 'Continue';
    var doneDisabled = requireConfirm ? ' disabled' : '';
    var existing = document.getElementById('recovery-backup-overlay');
    if (existing) existing.remove();
    var overlay = document.createElement('div');
    overlay.id = 'recovery-backup-overlay';
    overlay.className = 'hw-unlock-overlay';
    overlay.style.display = 'flex';
    overlay.innerHTML =
        '<div class="hw-unlock-card recovery-backup-card" role="dialog" aria-modal="true" aria-labelledby="recovery-backup-title">' +
            '<div class="hw-unlock-title" id="recovery-backup-title">' + escapeHtml(opts.title || 'Backup Phrase') + '</div>' +
            '<p class="recovery-warn">' + (opts.warn || defaultWarn) + '</p>' +
            '<div class="hw-mnemonic-shell" id="recovery-backup-shell">' +
                '<div class="hw-mnemonic-grid" id="recovery-backup-grid" aria-hidden="true">' + grid + '</div>' +
                '<button type="button" class="hw-mnemonic-cover" id="recovery-backup-cover" aria-label="Reveal recovery phrase">' +
                    '<svg viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7-10-7-10-7z"/><circle cx="12" cy="12" r="3"/></svg>' +
                    '<span>Tap to reveal phrase</span>' +
                '</button>' +
            '</div>' +
            '<button type="button" class="nr-btn nr-btn-ghost w-full recovery-backup-copy" id="recovery-backup-copy">' +
                '<svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>' +
                '<span>Copy phrase</span>' +
            '</button>' +
            confirmHtml +
            '<button type="button" class="nr-btn w-full recovery-backup-done" id="recovery-backup-done"' + doneDisabled + '>' + escapeHtml(doneLabel) + '</button>' +
        '</div>';
    document.body.appendChild(overlay);

    var cover = document.getElementById('recovery-backup-cover');
    var shell = document.getElementById('recovery-backup-shell');
    var gridEl = document.getElementById('recovery-backup-grid');
    if (cover) {
        cover.addEventListener('click', function() {
            cover.style.display = 'none';
            if (shell) shell.classList.add('revealed');
            if (gridEl) gridEl.setAttribute('aria-hidden', 'false');
        });
        setTimeout(function() { cover.focus(); }, 50);
    }

    var copyBtn = document.getElementById('recovery-backup-copy');
    if (copyBtn) {
        copyBtn.addEventListener('click', function() {
            if (!navigator.clipboard) {
                showToast('Clipboard is not available', 'toast-orange', 2000);
                return;
            }
            navigator.clipboard.writeText(mnemonic).then(function() {
                showCopyConfirmationToast('Recovery phrase');
            }).catch(function() {
                showToast('Could not copy phrase', 'toast-orange', 2000);
            });
        });
    }

    var confirm = document.getElementById('recovery-backup-confirm');
    var done = document.getElementById('recovery-backup-done');
    if (confirm && done) {
        confirm.addEventListener('change', function() {
            done.disabled = !confirm.checked;
        });
    }
    if (done) done.addEventListener('click', function() {
        if (done.disabled) return;
        overlay.remove();
        if (typeof onDone === 'function') onDone();
    });
}
window.showRecoveryPhraseBackup = showRecoveryPhraseBackup;

// Re-display a software identity's recovery phrase. Passcode-protected identities
// prompt for the passcode (decrypts from the vault); otherwise the plaintext
// sidecar is read directly. Hardware identities never reach here (no stored phrase).
function viewRecoveryPhrase(target) {
    if (!target || target.is_hardware) return;
    var viewOpts = {
        title: 'Recovery Phrase',
        warn: 'These ' + RECOVERY_PHRASE_WORDS + ' words restore this identity on any device. Keep them secret and ' +
            'offline — anyone with them controls your identity. Make sure no one can see your screen.',
        button: 'Done',
        requireConfirm: false
    };
    if (target.passcode_protected) {
        showIdentityModal('View Recovery Phrase',
            '<div class="modal-field"><label>Passcode</label>' +
                '<input type="password" id="reveal-passcode-input" class="modal-input" maxlength="128" autocomplete="off"></div>' +
            '<p class="recovery-warn">Enter your passcode to reveal the ' + RECOVERY_PHRASE_WORDS + '-word recovery phrase for this identity.</p>' +
            '<div class="modal-error" id="reveal-error" style="display:none"></div>',
            function() {
                var pw = document.getElementById('reveal-passcode-input').value;
                var errEl = document.getElementById('reveal-error');
                if (!pw) { if (errEl) { errEl.textContent = 'Enter your passcode.'; errEl.style.display = ''; } return; }
                return RS.invoke('reveal_identity_mnemonic', { args: { hash: target.hash, passcode: pw } }).then(function(data) {
                    closeIdentityModal();
                    showRecoveryPhraseBackup(data && data.mnemonic, null, viewOpts);
                }).catch(function(err) {
                    if (errEl) { errEl.textContent = (err && err.message) || 'Could not reveal phrase'; errEl.style.display = ''; }
                });
            }
        );
        var btn = document.getElementById('identity-modal-confirm');
        if (btn) { btn.textContent = 'Reveal'; btn.dataset.baseLabel = 'Reveal'; }
    } else {
        RS.invoke('reveal_identity_mnemonic', { args: { hash: target.hash } }).then(function(data) {
            showRecoveryPhraseBackup(data && data.mnemonic, null, viewOpts);
        }).catch(function(err) {
            showToast((err && err.message) || 'Could not reveal phrase', 'toast-red', 3000);
        });
    }
}
window.viewRecoveryPhrase = viewRecoveryPhrase;

function openRestorePhraseModal(fromSetup) {
    showIdentityModal('Restore from Recovery Phrase',
        '<div class="modal-field">' +
            '<label>Recovery Phrase</label>' +
            '<textarea id="restore-phrase-input" class="modal-input restore-phrase-textarea" rows="3" autocomplete="off" autocapitalize="off" spellcheck="false" placeholder="Enter your ' + RECOVERY_PHRASE_WORDS + '-word recovery phrase, separated by spaces"></textarea>' +
            '<span class="restore-phrase-count" id="restore-phrase-count">0 / ' + RECOVERY_PHRASE_WORDS + ' words</span>' +
        '</div>' +
        '<div class="modal-field">' +
            '<label>Display Name <span class="text-xs">(optional)</span></label>' +
            '<input type="text" id="restore-phrase-nickname" class="modal-input" placeholder="e.g. Rat King" maxlength="32">' +
        '</div>' +
        identityPasscodeOptionHtml('restore-phrase') +
        '<p class="recovery-warn">Restored software identities can be encrypted on this device with an optional passcode.</p>' +
        '<div class="modal-error" id="restore-phrase-error" style="display:none;"></div>',
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
                if (errEl) { errEl.textContent = 'Recovery phrase must be exactly ' + RECOVERY_PHRASE_WORDS + ' words.'; errEl.style.display = ''; }
                return;
            }
            if (errEl) errEl.style.display = 'none';
            return RS.invoke('restore_seed_identity', {
                args: { phrase: phrase, nickname: nickname }
            }).then(function(data) {
                var hash = data && (data.hash || data.identity_hash);
                var protect = passcode ? protectIdentityWithPasscode(hash, passcode).catch(function(err) {
                    var msg = (err && err.message) ? err.message : 'Could not encrypt identity with passcode';
                    showToast('Identity restored, but passcode setup failed: ' + msg, 'toast-red', 7000);
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
                            window.location.href = '/#dashboard';
                            window.location.reload();
                        }
                        return;
                    }
                    showToast('Identity restored', 'toast-green', 3000);
                    loadIdentities();
                });
            }).catch(function(err) {
                var msg = (err && err.message) ? err.message : 'Failed to restore identity';
                if (errEl) { errEl.textContent = msg; errEl.style.display = ''; }
                else showToast(msg, 'toast-red', 4000);
            });
        }
    );

    var ta = document.getElementById('restore-phrase-input');
    var countEl = document.getElementById('restore-phrase-count');
    if (ta && countEl) {
        ta.addEventListener('input', function() {
            var words = ta.value.trim().split(/\s+/).filter(Boolean);
            countEl.textContent = words.length + ' / ' + RECOVERY_PHRASE_WORDS + ' words';
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
                if (err && err.cancelled) {
                    showToast('Identity import cancelled', 'toast-orange', 2500);
                } else {
                    showToast(err && err.message ? err.message : 'Identity import failed', 'toast-red', 4000);
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
    });
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
        showToast(err && err.message ? err.message : 'Identity import failed', 'toast-red', 4000);
    });
}

function handleImportBackupPayload(fileName, fileSize, b64, expectedFormat) {
    if (!b64) return Promise.reject(new Error('Identity import is empty'));
    var fromSetup = !!window._identityImportFromSetup;
    var importFormat = expectedFormat || window._identityImportFormat || 'ratspeak';
    return RS.invoke('api_preview_identity_import_base64', {
        args: { key: b64, nickname: '' }
    }).then(function(preview) {
        if (importFormat === 'ratspeak' && preview.format !== 'ratspeak.identity.v1') {
            resetPendingIdentityImport();
            showToast('That is a Reticulum identity. Choose Reticulum Identity Key import.', 'toast-red', 4000);
            return;
        }
        if (importFormat === 'reticulum' && preview.format === 'ratspeak.identity.v1') {
            resetPendingIdentityImport();
            showToast('That is a Ratspeak backup. Choose Ratspeak Identity Backup import.', 'toast-red', 4000);
            return;
        }
        var duplicate = false;
        for (var i = 0; i < identityList.length; i++) {
            if (identityList[i].hash === preview.identity_hash) duplicate = true;
        }
        if (duplicate) {
            resetPendingIdentityImport();
            showToast('Identity is already on this device', 'toast-red', 3000);
            return;
        }
        var activateHtml = fromSetup ? '' :
            '<label class="identity-import-activate"><input type="checkbox" id="identity-import-activate"> <span>Activate after import</span></label>';
        showIdentityModal('Import Identity',
            '<div class="modal-field">' +
                '<label>File</label>' +
                '<span class="modal-value text-xs">' + escapeHtml(fileName || 'identity file') + ' (' + (fileSize || 0) + ' bytes)</span>' +
            '</div>' +
            '<div class="identity-import-preview">' +
                '<div class="identity-field"><span class="identity-label">LXMF Address</span><span class="identity-value mono">' + escapeHtml(preview.lxmf_hash || '') + '</span></div>' +
                '<div class="identity-field"><span class="identity-label">Identity Hash</span><span class="identity-value mono">' + escapeHtml(preview.identity_hash || '') + '</span></div>' +
            '</div>' +
            '<div class="modal-field">' +
                '<label>Display Name</label>' +
                '<input type="text" id="identity-modal-nickname" class="modal-input" placeholder="e.g. Rat King" maxlength="32">' +
            '</div>' +
            activateHtml,
            function() {
                var nickname = document.getElementById('identity-modal-nickname').value.trim();
                var activateInput = document.getElementById('identity-import-activate');
                var activate = fromSetup || !!(activateInput && activateInput.checked);
                return RS.invoke('api_import_identity_base64', {
                    args: { key: b64, nickname: nickname },
                }).then(function(data) {
                    showToast('Identity imported', 'toast-green', 3000);
                    closeIdentityModal();
                    loadIdentities();
                    if (fromSetup || window._identityImportFromSetup) {
                        resetPendingIdentityImport();
                        if (typeof completeSetupAfterIdentityImport === 'function') {
                            completeSetupAfterIdentityImport(data);
                        } else {
                            window.location.href = '/#dashboard';
                            window.location.reload();
                        }
                        return;
                    }
                    window._identityImportFormat = null;
                    if (activate && data && data.hash) switchToIdentity(data.hash);
                }).catch(function(err) {
                    resetPendingIdentityImport();
                    showToast(err && err.message ? err.message : 'Failed to import identity', 'toast-red', 3000);
                });
            }
        );
    });
}

function exportActiveIdentity() {
    exportIdentityBackup(activeIdentityHash);
}

function exportIdentityPayload(hash, format) {
    if (format === 'reticulum') {
        return RS.invoke('api_export_identity_reticulum_base64', { hashHex: hash }).then(function(data) {
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
        return RS.invoke('api_export_identity_reticulum_base32', { hashHex: hash }).then(function(data) {
            return {
                bytes: base64ToBytes(data.data_base64),
                base64: data.data_base64,
                fileName: data.file_name || (hash.substring(0, 16) + '-reticulum-identity-key-base32.txt'),
                mimeType: 'text/plain',
                label: 'Reticulum base32 key'
            };
        });
    }
    return RS.invoke('api_export_identity_backup_base64', { hashHex: hash }).then(function(data) {
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
    return 'This exports an unencrypted Ratspeak identity backup for ' + targetName + '. Anyone with this file can use the identity. It includes identity metadata, but not messages or app settings.';
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
    var chosenFormat = null;
    chooseIdentityExportFormat().then(function(format) {
        if (!format) return null;
        chosenFormat = format;
        return rsConfirm({
            title: 'Export Private Identity',
            message: exportWarningForFormat(format, targetName),
            confirmText: 'Export',
            danger: true
        });
    }).then(function(ok) {
        if (!ok) return;
        return exportIdentityPayload(hash, chosenFormat || 'ratspeak');
    }).then(function(payload) {
        if (!payload) return;
        return saveBytesToUserFile(payload.bytes, payload.fileName, payload.mimeType, payload.base64)
            .then(function(result) {
                if (result) result.label = payload.label;
                return result;
            });
    }).then(function(result) {
        if (!result) return;
        if (result.method === 'android-document') {
            showToast((result.label || 'Identity export') + ' saved', 'toast-green', 3000);
        } else {
            showToast('Save prompt opened for ' + result.fileName, 'toast-blue', 4000);
        }
    }).catch(function(err) {
        if (err && err.cancelled) {
            showToast('Identity export cancelled', 'toast-orange', 2500);
        } else {
            showToast(err && err.message ? err.message : 'Export failed', 'toast-red', 3000);
        }
    });
}

function openIdentityActions(hash) {
    var target = identityByHash(hash);
    if (!target) return;

    var choices = [];
    if (!target.is_active) {
        choices.push({ label: 'Switch to Identity', value: 'switch' });
    }
    if (!target.is_hardware) {
        choices.push({ label: 'Export Identity', value: 'export', hint: 'Ratspeak or Reticulum format.' });
        if (target.has_mnemonic) {
            choices.push({ label: 'View Recovery Phrase', value: 'view-phrase', hint: 'Show this identity’s 12-word phrase.' });
        }
        if (target.passcode_protected) {
            choices.push({ label: 'Change Passcode', value: 'passcode-change' });
            choices.push({ label: 'Remove Passcode', value: 'passcode-remove', danger: true });
        } else {
            choices.push({ label: 'Set Passcode', value: 'passcode-set', hint: 'Encrypt this identity at rest; required when you open Ratspeak.' });
        }
    }
    choices.push({ label: 'Share Contact Card', value: 'share' });
    if (!isOriginalIdentity(target.hash) && (!target.is_active || identityList.length > 1)) {
        choices.push({ label: target.is_hardware ? 'Remove Identity' : 'Delete Identity', value: 'delete', danger: true });
    }

    rsChoice({
        title: identityDisplayName(target),
        message: target.lxmf_hash || target.hash || '',
        choices: choices
    }).then(function(choice) {
        if (choice === 'switch') switchToIdentity(target.hash);
        if (choice === 'export') exportIdentityBackup(target.hash);
        if (choice === 'view-phrase') viewRecoveryPhrase(target);
        if (choice === 'passcode-set') openSetPasscodeModal(target, false);
        if (choice === 'passcode-change') openSetPasscodeModal(target, true);
        if (choice === 'passcode-remove') openRemovePasscodeModal(target);
        if (choice === 'share') {
            if (typeof openIdentityShareScreen === 'function') openIdentityShareScreen(target.hash);
            else shareAddress(target.lxmf_hash || target.hash || '', identityDisplayName(target));
        }
        if (choice === 'delete') deleteIdentityByHash(target.hash);
    });
}

// Add or change a passcode (at-rest encryption) on a software identity.
function openSetPasscodeModal(target, isChange) {
    var body =
        (isChange
            ? '<div class="modal-field"><label>Current Passcode</label>' +
              '<input type="password" id="passcode-current" class="modal-input" maxlength="128" autocomplete="off"></div>'
            : '') +
        '<div class="modal-field"><label>' + (isChange ? 'New Passcode' : 'Passcode') + '</label>' +
            '<input type="password" id="passcode-new" class="modal-input" maxlength="128" autocomplete="off" placeholder="At least 6 characters"></div>' +
        '<div class="modal-field"><label>Confirm Passcode</label>' +
            '<input type="password" id="passcode-confirm" class="modal-input" maxlength="128" autocomplete="off"></div>' +
        '<p class="recovery-warn">This identity will be encrypted on this device and require the passcode each time you open Ratspeak. There is <strong>no recovery</strong> if you forget it — keep your 12-word phrase as a backup.</p>' +
        '<div class="modal-error" id="passcode-error" style="display:none"></div>';
    showIdentityModal(isChange ? 'Change Passcode' : 'Set Passcode', body, function() {
        var cur = isChange ? document.getElementById('passcode-current').value : '';
        var pw = document.getElementById('passcode-new').value;
        var confirm = document.getElementById('passcode-confirm').value;
        var errEl = document.getElementById('passcode-error');
        if (pw.length < 6) {
            if (errEl) { errEl.textContent = 'Passcode must be at least 6 characters.'; errEl.style.display = ''; }
            return;
        }
        if (pw !== confirm) {
            if (errEl) { errEl.textContent = "Passcodes don't match."; errEl.style.display = ''; }
            return;
        }
        var args = { hash: target.hash, passcode: pw };
        if (isChange) args.current = cur;
        return RS.invoke('set_identity_passcode', { args: args }).then(function() {
            closeIdentityModal();
            showToast('Passcode set', 'toast-green', 3000);
            loadIdentities();
        }).catch(function(err) {
            if (errEl) { errEl.textContent = (err && err.message) || 'Could not set passcode'; errEl.style.display = ''; }
        });
    });
    var btn = document.getElementById('identity-modal-confirm');
    if (btn) { var l = isChange ? 'Change' : 'Set'; btn.textContent = l; btn.dataset.baseLabel = l; }
}

function openRemovePasscodeModal(target) {
    showIdentityModal('Remove Passcode',
        '<div class="modal-field"><label>Passcode</label>' +
            '<input type="password" id="passcode-remove-input" class="modal-input" maxlength="128" autocomplete="off"></div>' +
        '<p class="recovery-warn">This decrypts the identity on this device — it will no longer require a passcode when you open Ratspeak.</p>' +
        '<div class="modal-error" id="passcode-error" style="display:none"></div>',
        function() {
            var pw = document.getElementById('passcode-remove-input').value;
            var errEl = document.getElementById('passcode-error');
            if (!pw) {
                if (errEl) { errEl.textContent = 'Enter your passcode.'; errEl.style.display = ''; }
                return;
            }
            return RS.invoke('remove_identity_passcode', { args: { hash: target.hash, passcode: pw } }).then(function() {
                closeIdentityModal();
                showToast('Passcode removed', 'toast-green', 3000);
                loadIdentities();
            }).catch(function(err) {
                if (errEl) { errEl.textContent = (err && err.message) || 'Could not remove passcode'; errEl.style.display = ''; }
            });
        }
    );
}

function removeSelectedIdentity() {
    deleteIdentityByHash(selectedIdentityHash);
}

function deleteActiveIdentity() {
    deleteIdentityByHash(activeIdentityHash);
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

    rsConfirm({
        title: 'Delete Identity',
        message: 'This identity will be removed from your device completely, and can not be recovered. Are you sure you want to delete it?',
        confirmText: 'Delete',
        danger: true
    }).then(function(ok) {
        if (!ok) return;
        return rsChoice({
            title: 'Delete Identity Data?',
            message: 'Do you also want to remove any stored contacts, messages, or other data related to this identity?',
            choices: [
                { label: 'Yes, delete everything', value: 'cascade', danger: true, hint: 'Contacts, messages, games, and all other data will be permanently deleted.' },
                { label: 'No, just the identity', value: 'keep', hint: 'Data will be preserved and reappear if this identity is re-imported.' }
            ]
        });
    }).then(function(choice) {
        if (!choice) return;
        var cascade = (choice === 'cascade');
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
                showToast('Identity deleted', 'toast-green', 3000);
                selectedIdentityHash = null;
                loadIdentities();
            })
            .catch(function(err) {
                showToast(err && err.message ? err.message : 'Failed to delete identity', 'toast-red', 3000);
            });
    });
}

function showIdentityModal(title, bodyHtml, onConfirm, confirmClass) {
    var modal = document.getElementById('identity-modal');
    if (!modal) return;

    document.getElementById('identity-modal-title').textContent = title;
    document.getElementById('identity-modal-body').innerHTML = bodyHtml;

    var confirmBtn = document.getElementById('identity-modal-confirm');
    confirmBtn.className = confirmClass || 'nr-btn';
    confirmBtn.disabled = false;
    var baseLabel = title.indexOf('Delete') !== -1 ? 'Delete' : (title.indexOf('Remove') !== -1 ? 'Remove' : (title.indexOf('Import') !== -1 ? 'Import' : (title.indexOf('Restore') !== -1 ? 'Restore' : 'Create')));
    confirmBtn.textContent = baseLabel;
    confirmBtn.dataset.baseLabel = baseLabel;
    confirmBtn.onclick = function() {
        if (confirmBtn.disabled) return;
        confirmBtn.disabled = true;
        confirmBtn.textContent = 'Working\u2026';
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

    if (RS.ui && typeof RS.ui.openExistingSheet === 'function') {
        RS.ui.openExistingSheet('identity-modal', 'identity-modal-overlay');
    } else {
        var overlay = document.getElementById('identity-modal-overlay');
        modal.classList.add('open');
        if (overlay) overlay.classList.add('active');
    }

    modal.onkeydown = function(e) {
        if (e.key === 'Enter' && e.target.tagName === 'INPUT') {
            e.preventDefault();
            var btn = document.getElementById('identity-modal-confirm');
            if (btn) btn.click();
        }
    };

    setTimeout(function() {
        var input = modal.querySelector('.modal-input');
        if (input && !isMobile()) input.focus();
    }, 100);
}

function closeIdentityModal() {
    var modal = document.getElementById('identity-modal');
    var overlay = document.getElementById('identity-modal-overlay');
    var title = document.getElementById('identity-modal-title');
    if (title && title.textContent.indexOf('Import') !== -1) {
        resetPendingIdentityImport();
    }
    if (RS.ui && typeof RS.ui.closeExistingSheet === 'function') {
        RS.ui.closeExistingSheet('identity-modal', 'identity-modal-overlay');
    } else {
        if (modal) modal.classList.remove('open');
        if (overlay) overlay.classList.remove('active');
    }
    var confirmBtn = document.getElementById('identity-modal-confirm');
    if (confirmBtn) {
        confirmBtn.disabled = false;
        if (confirmBtn.dataset.baseLabel) confirmBtn.textContent = confirmBtn.dataset.baseLabel;
    }
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

    var identityAddBtn = document.getElementById('identity-add-btn');
    if (identityAddBtn) identityAddBtn.addEventListener('click', createNewIdentity);

    var identityImportBtn = document.getElementById('identity-import-btn');
    if (identityImportBtn) identityImportBtn.addEventListener('click', importIdentity);

    var identityExportBtn = document.getElementById('identity-export-btn');
    if (identityExportBtn) identityExportBtn.addEventListener('click', exportActiveIdentity);

    var identityCopyBtn = document.getElementById('identity-copy-address-btn');
    if (identityCopyBtn) identityCopyBtn.addEventListener('click', function() {
        var active = activeIdentity();
        var address = active && (active.lxmf_hash || active.hash);
        if (!address) return;
        if (!navigator.clipboard) {
            shareAddress(address, active.display_name || active.nickname || '');
            return;
        }
        navigator.clipboard.writeText(address).then(function() {
            showCopyConfirmationToast('Address');
        }).catch(function() {});
    });

    var identityShareBtn = document.getElementById('identity-share-address-btn');
    if (identityShareBtn) identityShareBtn.addEventListener('click', function() {
        var active = activeIdentity();
        if (!active) return;
        shareAddress(active.lxmf_hash || active.hash || '', active.display_name || active.nickname || '');
    });

    var identityDeleteBtn = document.getElementById('identity-delete-btn');
    if (identityDeleteBtn) identityDeleteBtn.addEventListener('click', deleteActiveIdentity);

    var modalClose = document.getElementById('identity-modal-close');
    if (modalClose) modalClose.addEventListener('click', closeIdentityModal);

    var modalCancel = document.getElementById('identity-modal-cancel');
    if (modalCancel) modalCancel.addEventListener('click', closeIdentityModal);

    if (typeof initSheetSwipeDismiss === 'function') {
        initSheetSwipeDismiss('identity-modal', 'identity-modal-overlay', closeIdentityModal);
    }

    document.addEventListener('keydown', function(e) {
        if (e.key === 'Escape') {
            var modal = document.getElementById('identity-modal');
            if (modal && modal.classList.contains('open')) {
                closeIdentityModal();
            }
        }
    });
});

RS.listen('identity_switching', function() {
    window._identitySwitchInProgress = true;
    if (typeof clearNetworkInterfaceCaches === 'function') {
        clearNetworkInterfaceCaches({ render: true });
    }
    if (typeof _clearConnectTimeout === 'function') _clearConnectTimeout();
    if (typeof clearConnectPublicPending === 'function') clearConnectPublicPending();
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
    if (typeof lxmfContacts !== 'undefined') lxmfContacts = [];
    if (typeof contactIdentityStatus !== 'undefined') contactIdentityStatus = {};

    if (typeof lxmfConversation !== 'undefined') lxmfConversation = [];
    if (typeof _conversationCache !== 'undefined') {
        for (var k in _conversationCache) delete _conversationCache[k];
    }
    if (typeof _cacheLru !== 'undefined') _cacheLru = [];

    if (typeof lxmfActiveContact !== 'undefined') lxmfActiveContact = null;
    if (typeof lxmfPendingFile !== 'undefined') lxmfPendingFile = null;
    if (typeof lxmfIdentity !== 'undefined') lxmfIdentity = null;
    if (typeof _ghostConversationHash !== 'undefined') _ghostConversationHash = null;
    if (typeof _replyTarget !== 'undefined') _replyTarget = null;
    if (typeof _msgReactions !== 'undefined') _msgReactions = {};
    if (typeof _lxmfDrafts !== 'undefined') _lxmfDrafts = {};

    if (typeof lxmfIdentityHash !== 'undefined') lxmfIdentityHash = data.hash;

    if (typeof events !== 'undefined') events = [];
    if (typeof activityLog !== 'undefined') activityLog = [];

    var msgList = document.getElementById('lxmf-messages');
    if (msgList) msgList.innerHTML = '<div class="lxmf-empty">Select a contact to view conversation.</div>';
    var chatHeader = document.getElementById('lxmf-chat-header');
    if (chatHeader) chatHeader.style.display = 'none';

    if (typeof _conversationsFirstLoadDone !== 'undefined') _conversationsFirstLoadDone = false;
    if (typeof _lastConversationsLoad !== 'undefined') _lastConversationsLoad = 0;
    if (typeof loadConversations === 'function') loadConversations();

    if (typeof renderMergedConnections === 'function') renderMergedConnections();
    if (typeof refreshConnectPublicServers === 'function') refreshConnectPublicServers(null, { force: true });

    if (typeof gamesTabClear === 'function') gamesTabClear();

    if (typeof renderActivityFeed === 'function') renderActivityFeed();
    if (typeof renderLog === 'function') renderLog();

    setTimeout(function() { window._identitySwitchInProgress = false; }, 2000);

    showToast('Identity switched', 'toast-green', 3000);
});

// ---------------------------------------------------------------------------
// Hardware Key (YubiKey/Nitrokey PIV) identity flow
// ---------------------------------------------------------------------------

var HW_BADGE_ICON = '<svg class="identity-hardware-badge-icon" viewBox="0 0 24 24" aria-hidden="true"><path d="M14 7a4 4 0 1 0-3.9 5H15v3h3v3h3v-4l-3-3a4 4 0 0 0-1-4z"/><circle cx="7" cy="10" r="1.2"/></svg>';

var HW_DETECT_ERROR_COPY = "YubiKey not detected. Please make sure it's a YubiKey 5+ running the latest firmware.";

// Wizard state. `mnemonic` is held here only — never persisted, logged, or
// echoed to storage. Cleared on close/verify.
var _hwCtx = null;

var HW_STEP_IDS = [
    'hw-step-detect', 'hw-step-mode', 'hw-step-pin', 'hw-step-working',
    'hw-step-mnemonic', 'hw-step-verify', 'hw-step-restore', 'hw-step-import'
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
        _hwCtx.verify = null;
    }
    var grid = document.getElementById('hw-mnemonic-grid');
    if (grid) grid.innerHTML = '';
    var fields = document.getElementById('hw-verify-fields');
    if (fields) fields.innerHTML = '';
    ['hw-pin', 'hw-pin-confirm', 'hw-restore-pin', 'hw-restore-pin-confirm', 'hw-restore-phrase'].forEach(function(id) {
        var el = document.getElementById(id);
        if (el) el.value = '';
    });
}

function _hwOpenSheet() {
    if (RS.ui && typeof RS.ui.openExistingSheet === 'function') {
        RS.ui.openExistingSheet('hw-modal', 'hw-modal-overlay');
    } else {
        var modal = document.getElementById('hw-modal');
        var overlay = document.getElementById('hw-modal-overlay');
        if (modal) modal.classList.add('open');
        if (overlay) overlay.classList.add('active');
    }
}

function closeHardwareWizard() {
    _hwClearSecrets();
    if (RS.ui && typeof RS.ui.closeExistingSheet === 'function') {
        RS.ui.closeExistingSheet('hw-modal', 'hw-modal-overlay');
    } else {
        var modal = document.getElementById('hw-modal');
        var overlay = document.getElementById('hw-modal-overlay');
        if (modal) modal.classList.remove('open');
        if (overlay) overlay.classList.remove('active');
    }
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
    _hwSetTitle('Hardware Key');
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
    if (device.firmware) parts.push('Firmware ' + device.firmware);
    return '<div class="hw-device-card">' +
        '<span class="hw-device-icon">' + HW_BADGE_ICON + '</span>' +
        '<span class="hw-device-meta">' +
            '<span class="hw-device-name">' + escapeHtml(model) + '</span>' +
            (parts.length ? '<span class="hw-device-detail">' + escapeHtml(parts.join('  ·  ')) + '</span>' : '') +
        '</span>' +
    '</div>';
}

function _hwShowModeStep() {
    _hwSetTitle('Backup Mode');
    var deviceEl = document.getElementById('hw-mode-device');
    if (deviceEl) deviceEl.innerHTML = _hwDeviceSummaryHtml(_hwCtx.device);
    _hwShowStep('hw-step-mode');
}

function _hwShowImportStep() {
    _hwSetTitle('Use Existing Key');
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
    _hwSetTitle('Working');
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
                showToast((e && e.message) ? e.message : 'Failed to reset security key', 'toast-red', 7000);
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
    showToast(msg, 'toast-red', 5000);
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
    _hwSetTitle('Backup Phrase');
    var words = String(mnemonic || '').trim().split(/\s+/).filter(Boolean);
    _hwCtx.mnemonic = mnemonic;
    _hwCtx.mnemonicWords = words;

    var grid = document.getElementById('hw-mnemonic-grid');
    if (grid) {
        grid.innerHTML = words.map(function(word, i) {
            return '<div class="hw-mnemonic-word">' +
                '<span class="hw-mnemonic-index">' + (i + 1) + '</span>' +
                '<span class="hw-mnemonic-text">' + escapeHtml(word) + '</span>' +
            '</div>';
        }).join('');
    }
    var cover = document.getElementById('hw-mnemonic-cover');
    if (cover) cover.style.display = '';
    var shell = document.querySelector('.hw-mnemonic-shell');
    if (shell) shell.classList.remove('revealed');

    var confirmChk = document.getElementById('hw-mnemonic-confirm');
    if (confirmChk) confirmChk.checked = false;
    var continueBtn = document.getElementById('hw-mnemonic-continue-btn');
    if (continueBtn) continueBtn.disabled = true;

    _hwShowStep('hw-step-mnemonic');
}

// Pick two distinct word positions for the verify step.
function _hwPickVerifyPositions(count) {
    var a = Math.floor(Math.random() * count);
    var b;
    do { b = Math.floor(Math.random() * count); } while (b === a && count > 1);
    return a <= b ? [a, b] : [b, a];
}

function _hwShowVerify() {
    _hwSetTitle('Confirm Backup');
    var words = _hwCtx.mnemonicWords || [];
    if (words.length < 2) { _hwFinish(_hwCtx.result || {}); return; }
    var positions = _hwPickVerifyPositions(words.length);
    _hwCtx.verify = positions;

    var fields = document.getElementById('hw-verify-fields');
    if (fields) {
        fields.innerHTML = positions.map(function(pos, idx) {
            var ordinal = pos + 1;
            return '<div class="modal-field"' + (idx > 0 ? ' style="margin-top:10px;"' : '') + '>' +
                '<label>Word #' + ordinal + '</label>' +
                '<input type="text" class="modal-input hw-verify-input" data-pos="' + pos + '" autocomplete="off" autocorrect="off" autocapitalize="none" spellcheck="false" placeholder="Enter word ' + ordinal + '">' +
            '</div>';
        }).join('');
    }
    var err = document.getElementById('hw-verify-error');
    if (err) err.style.display = 'none';
    _hwShowStep('hw-step-verify');
    setTimeout(function() {
        var first = document.querySelector('#hw-verify-fields .hw-verify-input');
        if (first && !isMobile()) first.focus();
    }, 120);
}

function _hwVerifyAndFinish() {
    var words = _hwCtx.mnemonicWords || [];
    var inputs = document.querySelectorAll('#hw-verify-fields .hw-verify-input');
    var ok = inputs.length > 0;
    Array.prototype.forEach.call(inputs, function(input) {
        var pos = parseInt(input.getAttribute('data-pos'), 10);
        var expected = (words[pos] || '').toLowerCase();
        if (input.value.trim().toLowerCase() !== expected) ok = false;
    });
    var err = document.getElementById('hw-verify-error');
    if (!ok) {
        if (err) { err.textContent = "Those words don't match. Check your written phrase and try again."; err.style.display = ''; }
        return;
    }
    if (err) err.style.display = 'none';
    var result = _hwCtx.result || {};
    _hwFinish(result);
}

// ---- Restore from phrase ----

function _hwBeginRestore() {
    if (!_hwCtx) _hwCtx = { fromSetup: false };
    _hwCtx.currentPin = null;
    _hwSetTitle('Restore from Phrase');
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
            showToast(msg, 'toast-red', 5000);
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
            showToast((e && e.message) ? e.message : 'Failed to register key', 'toast-red', 5000);
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
        var finishSetup = function() {
            if (typeof completeSetupAfterIdentityImport === 'function') {
                completeSetupAfterIdentityImport(result);
            } else {
                window.location.href = '/#dashboard';
                window.location.reload();
            }
        };
        if (pinForSetup) {
            RS.invoke('hw_stage_unlock', { pin: pinForSetup })
                .catch(function(err) {
                    window.RS.diag('warn', '[hardware] could not stage first-run unlock PIN:', err && err.message ? err.message : String(err));
                })
                .finally(finishSetup);
        } else {
            finishSetup();
        }
        return;
    }
    showToast('Hardware identity added', 'toast-green', 3000);
    loadIdentities();
    if (newHash) switchToIdentity(newHash);
}

function _hwCopyMnemonic() {
    var phrase = _hwCtx && _hwCtx.mnemonic;
    if (!phrase) return;
    if (navigator.clipboard) {
        navigator.clipboard.writeText(phrase).then(function() {
            showCopyConfirmationToast('Recovery phrase');
        }).catch(function() {});
    }
}

document.addEventListener('DOMContentLoaded', function() {
    // Hardware (YubiKey/PIV) identities are desktop-only for now: the `hardware`
    // feature + hw_* commands are gated off on mobile. Hide the entry points there.
    // TODO(ratkey-mobile): add the wrapped-session model — see HARDWARE_STATUS.md.
    var hideHardware = (typeof isMobile === 'function') && isMobile();

    var identityHwBtn = document.getElementById('identity-hardware-btn');
    if (identityHwBtn) {
        if (hideHardware) identityHwBtn.style.display = 'none';
        identityHwBtn.addEventListener('click', function() {
            openHardwareWizard({ fromSetup: false });
        });
    }

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
        revealCover.style.display = 'none';
        var shell = document.querySelector('.hw-mnemonic-shell');
        if (shell) shell.classList.add('revealed');
    });
    var copyBtn = document.getElementById('hw-mnemonic-copy-btn');
    if (copyBtn) copyBtn.addEventListener('click', _hwCopyMnemonic);
    var mnemonicConfirm = document.getElementById('hw-mnemonic-confirm');
    if (mnemonicConfirm) mnemonicConfirm.addEventListener('change', function() {
        var btn = document.getElementById('hw-mnemonic-continue-btn');
        if (btn) btn.disabled = !mnemonicConfirm.checked;
    });
    var mnemonicContinue = document.getElementById('hw-mnemonic-continue-btn');
    if (mnemonicContinue) mnemonicContinue.addEventListener('click', _hwShowVerify);

    var verifyBtn = document.getElementById('hw-verify-btn');
    if (verifyBtn) verifyBtn.addEventListener('click', _hwVerifyAndFinish);
    var verifyFields = document.getElementById('hw-verify-fields');
    if (verifyFields) verifyFields.addEventListener('keydown', function(e) {
        if (e.key === 'Enter') {
            e.preventDefault();
            _hwVerifyAndFinish();
        }
    });
    var verifyBack = document.getElementById('hw-verify-back-btn');
    if (verifyBack) verifyBack.addEventListener('click', function() {
        if (_hwCtx && _hwCtx.mnemonic) _hwShowMnemonic(_hwCtx.mnemonic);
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

    if (typeof initSheetSwipeDismiss === 'function') {
        initSheetSwipeDismiss('hw-modal', 'hw-modal-overlay', closeHardwareWizard);
    }

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
            showToast('Hardware identity removed', 'toast-green', 3000);
            selectedIdentityHash = null;
            loadIdentities();
        }).catch(function(err) {
            showToast((err && err.message) ? err.message : 'Failed to remove identity', 'toast-red', 3000);
        });
    });
}

// ---- Hardware identity unlock (PIN prompt) ----
// Shown when the active identity is hardware-backed and the token is locked
// (on boot, or after the auto-lock timeout). Unlocking re-inits the runtime.

var _hwLockedHash = null;
var _hwLockedKind = 'hardware'; // 'hardware' (YubiKey PIN) | 'passcode' (software)

// Launch/timeout unlock prompt. Handles both a hardware-key PIN and a software
// passcode (kind) — same flow, same `hw_unlock` command (the secret rides in `pin`).
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
        ? 'Enter your passcode to continue.'
        : 'Enter your YubiKey PIN to continue. Keep the key plugged in.';
    var inputAttrs = isPass
        ? 'type="password" autocomplete="off" maxlength="128" placeholder="Passcode"'
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
    RS.invoke('hw_unlock', { pin: secret }).then(function(res) {
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
            msg = 'Incorrect passcode.';
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
    RS.listen('hardware_locked', function(data) { showHwUnlock(data && data.hash, data && data.kind); });
}
