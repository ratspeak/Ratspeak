// Shared pieces of the peer identity panels: the Peers detail panel, the
// conversation About sheet, the saved-contact sheet, and the mobile peer sheet.

var PEER_DETAILS_OPEN_KEY = 'rs-peer-details-open';
var _peerDetailsOpen = (function() {
    try { return localStorage.getItem(PEER_DETAILS_OPEN_KEY) === '1'; } catch (e) { return false; }
})();

var PEER_ICON_ATTRS = 'viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"';
var PEER_ICON_COPY = '<svg class="peer-copy-glyph" ' + PEER_ICON_ATTRS + '><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>';
var PEER_ICON_CHEVRON = '<svg class="peer-details-chevron" width="16" height="16" ' + PEER_ICON_ATTRS + '><polyline points="9 18 15 12 9 6"></polyline></svg>';
var PEER_ICON_MESSAGE = '<svg ' + PEER_ICON_ATTRS + '><path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/></svg>';
var PEER_ICON_ADD = '<svg ' + PEER_ICON_ATTRS + '><path d="M16 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2"/><circle cx="8.5" cy="7" r="4"/><line x1="20" y1="8" x2="20" y2="14"/><line x1="23" y1="11" x2="17" y2="11"/></svg>';
var PEER_ICON_REMOVE = '<svg ' + PEER_ICON_ATTRS + '><path d="M16 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2"/><circle cx="8.5" cy="7" r="4"/><line x1="23" y1="11" x2="17" y2="11"/></svg>';
var PEER_ICON_BLOCK = '<svg ' + PEER_ICON_ATTRS + '><circle cx="12" cy="12" r="10"/><line x1="4.93" y1="4.93" x2="19.07" y2="19.07"/></svg>';

// Attributes for any button that copies a value; one delegated listener below serves them all.
function peerCopyAttrs(value, noun, label) {
    return ' type="button" dir="ltr" data-peer-copy="' + escapeHtml(value) + '" data-copy-noun="' + noun +
        '" aria-label="' + label + '"';
}

// nameHtml is empty for a peer with no name, so its hash shows once, as the address.
function peerNameHtml(className, nameHtml) {
    return nameHtml
        ? '<div class="' + className + '">' + nameHtml + '</div>'
        : '<div class="' + className + ' peer-name-placeholder">Unnamed peer</div>';
}

function peerAddressButtonHtml(hash) {
    return '<button class="peers-detail-hash"' +
        peerCopyAttrs(hash, 'LXMF address', 'Copy LXMF address') + ' title="Copy LXMF address">' +
        escapeHtml(hash) + '</button>';
}

function peerStatusChipHtml(peer) {
    var status = peer ? peer.status : 'unknown';
    var tone = (status === 'reachable' || status === 'direct') ? 'green' : (status === 'stale' ? 'warning' : 'muted');
    var dot = tone === 'green' ? 'green' : (tone === 'warning' ? 'orange' : 'gray');
    return '<span class="about-status-chip about-status-chip-' + tone + '">' +
        '<span class="dot ' + dot + '"></span>' + escapeHtml((peer && peer.activity_label) || 'Never seen') +
    '</span>';
}

function _peerIsRelayed(peer) {
    if (!peer || !peer.in_path) return false;
    if (peer.hops !== null && peer.hops !== undefined) return peer.hops > 1;
    return !!peer.via && peer.via !== peer.hash && peer.via !== peer.route_hash;
}

function peerRouteText(peer) {
    if (!peer || !peer.in_path) return 'No current path';
    if (!_peerIsRelayed(peer)) return 'Direct';
    if (peer.hops === null || peer.hops === undefined) return 'Relayed';
    return 'Relayed \u00b7 ' + peer.hops + ' hops';
}

function _peerHashButtonHtml(value, noun, label, full) {
    return '<button class="hash-copy peer-hash' + (full ? ' peer-hash--full' : '') + '"' +
        peerCopyAttrs(value, noun, label) +
        ' title="' + label + (full ? '' : ': ' + escapeHtml(value)) + '">' +
        escapeHtml(full ? value : shortHash(value, 8, 4)) + PEER_ICON_COPY +
    '</button>';
}

// opts: { muted, title (on the value), rowClass, rowTitle }
function _peerDetailRowHtml(label, valueHtml, opts) {
    opts = opts || {};
    return '<div class="about-row' + (opts.rowClass ? ' ' + opts.rowClass : '') + '"' +
            (opts.rowTitle ? ' title="' + escapeHtml(opts.rowTitle) + '"' : '') + '>' +
        '<span class="about-row-label">' + label + '</span>' +
        '<span class="about-row-value' + (opts.muted ? ' about-row-value-muted' : '') + '"' +
            (opts.title ? ' title="' + escapeHtml(opts.title) + '"' : '') + '>' + valueHtml + '</span>' +
    '</div>';
}

function _peerTimeRowHtml(label, ts) {
    return _peerDetailRowHtml(label, escapeHtml(formatLastHeard(ts)), {
        title: new Date(ts * 1000).toLocaleString()
    });
}

// The technical rows, in display order. opts: { trust }
function peerDetailRowsHtml(peer, opts) {
    opts = opts || {};
    var html = '<div class="about-section">';

    html += peer && peer.identity_hash
        ? _peerDetailRowHtml('Identity hash',
            _peerHashButtonHtml(peer.identity_hash, 'Identity hash', 'Copy identity hash', true),
            { rowClass: 'peer-identity-row' })
        : _peerDetailRowHtml('Identity hash',
            '<span title="Shown after this peer announces">Not known yet</span>', { muted: true });

    var inPath = !!(peer && peer.in_path);
    html += _peerDetailRowHtml('Route', escapeHtml(peerRouteText(peer)), { muted: !inPath });
    if (_peerIsRelayed(peer) && peer.via) {
        html += _peerDetailRowHtml('Relayed by',
            _peerHashButtonHtml(peer.via, 'Relay ID', 'Copy relay ID', false),
            { rowTitle: 'First relay on the path. Its ID changes when that relay restarts.' });
    }
    if (peer && peer.iface) {
        var compact = typeof _peerCompactIfaceLabel === 'function' ? _peerCompactIfaceLabel(peer.iface) : null;
        html += _peerDetailRowHtml('Interface',
            escapeHtml((compact || peer.iface) + (peer.iface_is_live ? '' : ' \u00b7 last known')));
    }
    if (inPath && peer.path_updated) html += _peerTimeRowHtml('Path updated', peer.path_updated);
    html += peer && peer.last_seen
        ? _peerTimeRowHtml('Last heard', peer.last_seen)
        : _peerDetailRowHtml('Last heard', 'No activity yet', { muted: true });
    if (peer && peer.first_seen) html += _peerTimeRowHtml('First heard', peer.first_seen);
    if (opts.trust) html += _peerDetailRowHtml('Trust', escapeHtml(opts.trust));

    return html + '</div>';
}

// The Details disclosure around the technical rows. The markup is always closed;
// syncPeerDetails applies the remembered state once it is in a container.
function peerDetailsHtml(id, peer, opts) {
    return '<div class="peer-details" data-open="false">' +
        '<button type="button" class="peer-details-toggle" aria-expanded="false" aria-controls="' + id + '">' +
            '<span>Details</span>' + PEER_ICON_CHEVRON +
        '</button>' +
        '<div class="peer-details-region" id="' + id + '" role="region" aria-label="Details" inert>' +
            peerDetailRowsHtml(peer, opts) +
        '</div>' +
    '</div>';
}

function syncPeerDetails(root) {
    root.querySelectorAll('.peer-details').forEach(function(box) {
        box.setAttribute('data-open', _peerDetailsOpen);
        box.querySelector('.peer-details-toggle').setAttribute('aria-expanded', _peerDetailsOpen);
        // Closed rows stay in the DOM for the slide; inert keeps them out of the tab order.
        box.querySelector('.peer-details-region').inert = !_peerDetailsOpen;
    });
}

// Call (when calling works on this device) and Message; ids are idPrefix + '-call-btn' / '-message-btn'.
function peerActionPairHtml(className, idPrefix, hash) {
    return '<div class="' + className + '">' +
        (typeof voiceActionButtonHtml === 'function' ? voiceActionButtonHtml(idPrefix + '-call-btn', hash) : '') +
        '<button type="button" class="nr-btn entity-action-btn" id="' + idPrefix + '-message-btn">' +
            PEER_ICON_MESSAGE + '<span>Message</span>' +
        '</button>' +
    '</div>';
}

function wirePeerActionPair(idPrefix, hash, before) {
    if (typeof wireVoiceActionButton === 'function') wireVoiceActionButton(idPrefix + '-call-btn', before);
    var messageBtn = document.getElementById(idPrefix + '-message-btn');
    if (!messageBtn) return;
    messageBtn.addEventListener('click', function() {
        if (typeof before === 'function') before();
        if (typeof openConversationWith === 'function') openConversationWith(hash);
    });
}

function _peerActionRowHtml(action, label, icon, danger) {
    return '<button type="button" class="action-popover-item' + (danger ? ' action-popover-item--danger' : '') +
            '" data-peer-action="' + action + '">' +
        '<span class="action-popover-item-icon">' + icon + '</span>' +
        '<span class="action-popover-item-label">' + label + '</span>' +
    '</button>';
}

function peerActionRowsHtml(isContact) {
    return '<div class="peer-action-rows">' +
        (isContact
            ? _peerActionRowHtml('remove', 'Remove from contacts', PEER_ICON_REMOVE, false)
            : _peerActionRowHtml('add', 'Add to contacts', PEER_ICON_ADD, false)) +
        _peerActionRowHtml('block', 'Block', PEER_ICON_BLOCK, true) +
    '</div>';
}

function peerPromptAddContact(hash, prefillName) {
    rsPrompt({ message: 'Contact name (optional):', placeholder: 'Display name', defaultValue: prefillName || '' }).then(function(name) {
        if (name === null) return;
        RS.invokeOrToast('add_contact', { args: { hash: hash, display_name: name.trim() || null } }, 'Could not add contact');
    });
}

function peerConfirmRemoveContact(hash, name) {
    rsConfirm({ message: 'Remove contact "' + name + '"?', danger: true, confirmText: 'Remove' }).then(function(ok) {
        if (!ok) return;
        RS.invokeOrToast('remove_contact', { hash: hash }, 'Could not remove contact');
    });
}

function peerConfirmBlock(hash, name, onBlocked) {
    rsConfirmWithCheckbox({
        message: 'Block "' + name + '"? They won\'t appear in your peers list and their messages will be hidden.',
        danger: true,
        confirmText: 'Block',
        checkboxLabel: 'Also block at the network layer (drop their packets entirely)',
        checkboxHelp: 'Stops their messages from reaching this device at all, instead of just hiding them. Useful for relay nodes. This affects all accounts on this device.',
        defaultChecked: false
    }).then(function(result) {
        if (!result.confirmed) return;
        RS.invokeOrToast('block_contact', { args: { hash: hash, escalate_to_blackhole: result.checked } }, 'Could not block contact')
            .then(function(resp) {
                if (resp && resp.blackhole_pending && typeof showToast === 'function') {
                    showToast('Blocked. Network filtering starts after their next announce.', 'toast-warning', 5000);
                }
            });
        if (typeof onBlocked === 'function') onBlocked();
    });
}

// opts: { name (display name, empty when none), before (runs first, e.g. closes the sheet), onBlocked }
function wirePeerActionRows(root, hash, opts) {
    if (!root) return;
    opts = opts || {};
    var label = opts.name || shortHash(hash, 8, 4);
    root.querySelectorAll('[data-peer-action]').forEach(function(btn) {
        btn.addEventListener('click', function() {
            if (typeof opts.before === 'function') opts.before();
            var action = btn.dataset.peerAction;
            if (action === 'add') peerPromptAddContact(hash, opts.name);
            else if (action === 'remove') peerConfirmRemoveContact(hash, label);
            else if (action === 'block') peerConfirmBlock(hash, label, opts.onBlocked);
        });
    });
}

document.addEventListener('click', function(e) {
    var copy = e.target.closest('[data-peer-copy]');
    if (copy) {
        RS.copyText(copy.dataset.peerCopy).then(function(ok) {
            if (ok && typeof showCopyConfirmationToast === 'function') showCopyConfirmationToast(copy.dataset.copyNoun);
        });
        return;
    }
    if (!e.target.closest('.peer-details-toggle')) return;
    _peerDetailsOpen = !_peerDetailsOpen;
    try { localStorage.setItem(PEER_DETAILS_OPEN_KEY, _peerDetailsOpen ? '1' : '0'); } catch (err) {}
    syncPeerDetails(document);
});
