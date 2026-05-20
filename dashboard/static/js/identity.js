var identityList = [];
var selectedIdentityHash = null;
var activeIdentityHash = null;

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
        }
    ];
}

function identityExportFormatChoices() {
    return [
        {
            label: 'Ratspeak Identity Backup',
            value: 'ratspeak',
            hint: '.rsi identity file with Ratspeak display-name metadata.'
        },
        {
            label: 'Reticulum Identity File',
            value: 'reticulum',
            hint: 'Raw 64-byte Reticulum private identity key.'
        },
        {
            label: 'Reticulum Base32 Key',
            value: 'reticulum-base32',
            hint: 'Base32 text form of the same Reticulum private identity key.'
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
        message: 'Choose the destination format.',
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
    if (window.File && navigator.canShare && navigator.share) {
        try {
            var file = new File([blob], fileName, { type: blob.type });
            if (navigator.canShare({ files: [file] })) {
                return navigator.share({ files: [file], title: fileName }).then(function() {
                    return { method: 'share', fileName: fileName };
                });
            }
        } catch (_) {}
    }
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
                _activeIdent.display_name || _activeIdent.nickname || ''
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
                    '<span class="identity-private-badge">Private Key</span>' +
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
            '<button class="identity-action-row" id="identity-export-detail-btn">' +
                '<span class="identity-action-icon"><svg viewBox="0 0 24 24"><path d="M12 21V9"/><path d="M7 14l5-5 5 5"/><path d="M5 21h14"/></svg></span>' +
                '<span>Export Identity</span>' +
            '</button>' +
            '<button class="identity-action-row" id="identity-share-address-btn">' +
                '<span class="identity-action-icon"><svg viewBox="0 0 24 24"><rect x="3" y="3" width="7" height="7"/><rect x="14" y="3" width="7" height="7"/><rect x="3" y="14" width="7" height="7"/><path d="M14 14h3v3h-3z"/><path d="M19 14h2"/><path d="M14 21h7v-2"/><path d="M19 17h2"/></svg></span>' +
                '<span>Share Contact Card</span>' +
            '</button>' +
            '<button class="identity-action-row identity-action-row--danger" id="identity-delete-btn"' + (canDelete ? '' : ' disabled aria-disabled="true"') + ' title="' + escapeHtml(deleteTitle) + '">' +
                '<span class="identity-action-icon"><svg viewBox="0 0 24 24"><path d="M3 6h18"/><path d="M8 6V4h8v2"/><path d="M19 6l-1 14H6L5 6"/></svg></span>' +
                '<span>Delete Identity</span>' +
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
        '</div>',
        function() {
            var nickname = document.getElementById('identity-modal-nickname').value.trim();
            return RS.invoke('api_create_identity', { args: { nickname: nickname } }).then(function() {
                showToast('Identity created', 'toast-green', 3000);
                closeIdentityModal();
            }).catch(function(err) {
                showToast(err && err.message ? err.message : 'Failed to create identity', 'toast-red', 3000);
            }).then(function() {
                // Refresh even on error — core may have created the row before timing out.
                loadIdentities();
            });
        }
    );
}

function importIdentity() {
    chooseIdentityImportFormat().then(function(format) {
        if (!format) {
            resetPendingIdentityImport();
            return;
        }
        window._identityImportFormat = format;
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
        return 'This exports only the private Reticulum identity key for ' + targetName + '. Reticulum tools can use it. Ratspeak messages and settings are not included.';
    }
    if (format === 'reticulum-base32') {
        return 'This exports the private Reticulum identity key for ' + targetName + ' as base32 text. Anyone with this text can use the identity. Ratspeak messages and settings are not included.';
    }
    return 'Anyone with this Ratspeak identity backup can use ' + targetName + '. It includes identity metadata, but not messages or app settings.';
}

function exportIdentityBackup(hash) {
    if (!hash) {
        showPreConditionToast('No active identity to export');
        return;
    }
    var target = identityByHash(hash);
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
        } else if (result.method === 'share') {
            showToast((result.label || 'Identity export') + ' handed to destination', 'toast-green', 3000);
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
    choices.push({ label: 'Export Identity', value: 'export', hint: 'Ratspeak or Reticulum format.' });
    choices.push({ label: 'Share Contact Card', value: 'share' });
    if (!isOriginalIdentity(target.hash) && (!target.is_active || identityList.length > 1)) {
        choices.push({ label: 'Delete Identity', value: 'delete', danger: true });
    }

    rsChoice({
        title: identityDisplayName(target),
        message: target.lxmf_hash || target.hash || '',
        choices: choices
    }).then(function(choice) {
        if (choice === 'switch') switchToIdentity(target.hash);
        if (choice === 'export') exportIdentityBackup(target.hash);
        if (choice === 'share') {
            if (typeof openIdentityShareScreen === 'function') openIdentityShareScreen(target.hash);
            else shareAddress(target.lxmf_hash || target.hash || '', identityDisplayName(target));
        }
        if (choice === 'delete') deleteIdentityByHash(target.hash);
    });
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
    var baseLabel = title.indexOf('Delete') !== -1 ? 'Delete' : (title.indexOf('Remove') !== -1 ? 'Remove' : (title.indexOf('Import') !== -1 ? 'Import' : 'Create'));
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
});

RS.listen('identity_switched', function(data) {
    // Suppress the redundant loadConversations() that lxmf_identity triggers —
    // emit_initial_state fires lxmf_identity right after identity_switched.
    window._identitySwitchInProgress = true;

    activeIdentityHash = data.hash;
    selectedIdentityHash = data.hash;

    if (data.lxmf_hash && typeof updateHeaderIdentity === 'function') {
        updateHeaderIdentity(data.lxmf_hash, data.display_name || '');
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
