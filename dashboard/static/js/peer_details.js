// People views shared by Peers and Contacts: the detail pane (desktop) and the
// person sheet (phone), with one vocabulary. The legacy helpers further down
// still serve the conversation About sheet and the Network peer sheet; their Details
// disclosure also folds the person sheet's facts.

// The person a hash names: the peer row (live route, heard times) and the saved contact.
function personModel(hash) {
    var peer = null, contact = null;
    var peers = (typeof PeersCache !== 'undefined' && PeersCache) ? PeersCache.enriched() : [];
    for (var i = 0; i < peers.length; i++) {
        if (peers[i].hash === hash) { peer = peers[i]; break; }
    }
    if (typeof lxmfContacts !== 'undefined' && Array.isArray(lxmfContacts)) {
        for (var j = 0; j < lxmfContacts.length; j++) {
            if (lxmfContacts[j] && lxmfContacts[j].hash === hash) { contact = lxmfContacts[j]; break; }
        }
    }
    var name = (contact && contact.display_name) || (peer && peer.display_name) || '';
    if (name === hash) name = '';
    var record = peer || contact || hash;
    return {
        hash: hash,
        peer: peer,
        contact: contact,
        isContact: !!contact || !!(peer && peer.is_contact),
        name: name,
        lastSeen: peer && peer.last_seen != null ? peer.last_seen : (contact ? contact.last_seen || null : null),
        firstSeen: peer && peer.first_seen != null ? peer.first_seen : (contact ? contact.first_seen || null : null),
        ratspeak: typeof ratspeakSupportsFeaturesFor === 'function' && ratspeakSupportsFeaturesFor(record),
        status: typeof ratspeakProfileStatusText === 'function' ? ratspeakProfileStatusText(record) : '',
        identityHash: (peer && peer.identity_hash) || ''
    };
}

var PERSON_CONNECTION_KINDS = { lora: 'Radio', ble: 'Bluetooth', tcp: 'Internet', host: 'Internet', local: 'Local' };

// Radio, Internet, Bluetooth, or Local: the kind of connection an interface name belongs to.
// The live row's type decides, then the saved entry, then the name.
function personConnectionKind(name) {
    if (!name) return '';
    var stats = (typeof lastStats !== 'undefined' && lastStats && lastStats.interface_stats && Array.isArray(lastStats.interface_stats.interfaces))
        ? lastStats.interface_stats.interfaces : [];
    for (var i = 0; i < stats.length; i++) {
        if (!stats[i] || stats[i].name !== name || typeof classifyInterface !== 'function') continue;
        var kind = PERSON_CONNECTION_KINDS[classifyInterface(stats[i])];
        if (kind) return kind;
    }
    var record = typeof getConfiguredInterfaceRecord === 'function' ? getConfiguredInterfaceRecord(name) : null;
    var saved = record && typeof interfaceSectionForConfigType === 'function' ? PERSON_CONNECTION_KINDS[interfaceSectionForConfigType(record.ifaceType)] : '';
    if (saved) return saved;
    var lower = String(name).toLowerCase();
    if (/^(ble[ _]?peer|ble mesh|bluetooth peer)/.test(lower)) return 'Bluetooth';
    if (/rnode|^(kiss|ax25|lora|androidusb|android_usb)/.test(lower)) return 'Radio';
    if (/^(local|shared|auto|udp)/.test(lower)) return 'Local';
    if (/^(tcp|backbone|i2p)/.test(lower) || lower.indexOf(':') > 0) return 'Internet';
    return '';
}

// The connection's own name, with Bluetooth Peer's internal names read as the product name.
function personConnectionName(name) {
    if (!name) return '';
    return /^(ble[ _]?peer|ble mesh)/i.test(String(name)) ? 'Bluetooth Peer' : String(name);
}

function personIsDirect(peer) {
    return !!(peer && peer.in_path && peer.hops != null && peer.hops <= 1);
}

function personRouteText(peer) {
    if (!peer || !peer.in_path) return 'No current path';
    if (peer.hops == null) return _peerIsRelayed(peer) ? 'Relayed' : 'Direct';
    return peer.hops <= 1 ? 'Direct' : 'Relayed · ' + peer.hops + ' hops';
}

// The one trailing word of a Peers row: the live connection, or the route when sorted by hops.
function personRowTrail(peer, sort) {
    if (!peer) return '';
    if (sort === 'hops') {
        if (!peer.in_path || peer.hops == null) return '';
        return peer.hops <= 1 ? 'Direct' : peer.hops + ' hops';
    }
    return peer.iface_is_live ? personConnectionKind(peer.iface) : '';
}

function personHeardOnce(model) {
    return !!(model.lastSeen && model.firstSeen && Math.abs(model.lastSeen - model.firstSeen) < 60);
}

function personHeardLine(model) {
    if (!model.lastSeen) return 'Not heard yet';
    if (personHeardOnce(model)) return 'Heard once, ' + RS.kit.time.sentence(model.lastSeen);
    return RS.kit.time.lastHeard(model.lastSeen);
}

// Signed numbers read with a true minus sign; SNR keeps the radio's quarter steps.
function _personSignedNumber(value, digits) {
    var text = digits ? String(Math.round(value * 100) / 100) : String(Math.round(value));
    return text.charAt(0) === '-' ? '−' + text.slice(1) : text;
}

// A direct radio peer's last packet; a relayed packet measures the relay, so nothing then.
function personSignalText(peer) {
    if (!peer || !peer.signal || !personIsDirect(peer) || !peer.iface_is_live) return '';
    if (personConnectionKind(peer.iface) !== 'Radio') return '';
    var text = _personSignedNumber(peer.signal.rssi) + ' dBm RSSI';
    if (peer.signal.snr != null) text += ' · ' + _personSignedNumber(peer.signal.snr, 2) + ' dB SNR';
    return text;
}

function _personTimeHtml(ts) {
    var text = RS.kit.time.sentence(ts);
    return '<span title="' + escapeHtml(RS.kit.time.full(ts)) + '">' + escapeHtml(text.charAt(0).toUpperCase() + text.slice(1)) + '</span>';
}

// rows: [label, valueHtml, { muted, title }]
function personDlHtml(rows, stacked) {
    return '<dl class="ui-dl' + (stacked ? ' ui-dl--stack' : '') + '">' + rows.map(function(r) {
        var o = r[2] || {};
        return '<dt>' + r[0] + '</dt><dd' + (o.muted ? ' class="is-muted"' : '') + (o.title ? ' title="' + escapeHtml(o.title) + '"' : '') + '>' + r[1] + '</dd>';
    }).join('') + '</dl>';
}

function personFactsHtml(model, idPrefix) {
    var peer = model.peer;
    var reach = [['Route', escapeHtml(personRouteText(peer)), { muted: !(peer && peer.in_path) }]];
    if (peer && _peerIsRelayed(peer) && peer.via) {
        reach.push(['Relayed by', RS.kit.hash.copy(peer.via, { form: 'fit', noun: 'Relay ID' }),
            { title: 'First relay on the path. Its ID changes when that relay restarts.' }]);
    }
    if (peer && peer.iface) {
        reach.push(['Connection', escapeHtml(personConnectionName(peer.iface)) +
            (peer.iface_is_live ? '' : ' <span class="ui-dl-hint">· last known</span>')]);
    }
    var signal = personSignalText(peer);
    if (signal) reach.push(['Last packet', escapeHtml(signal)]);
    var history = [];
    if (model.lastSeen && model.firstSeen && !personHeardOnce(model)) history.push(['First heard', _personTimeHtml(model.firstSeen)]);
    if (model.lastSeen && peer && peer.path_updated && Math.abs(peer.path_updated - model.lastSeen) > 60) {
        history.push(['Path updated', _personTimeHtml(peer.path_updated)]);
    }
    return '<section aria-labelledby="' + idPrefix + '-reach"><h3 class="ui-section-title" id="' + idPrefix + '-reach">Reachability</h3>' + personDlHtml(reach) + '</section>' +
        (history.length ? '<section aria-labelledby="' + idPrefix + '-hist"><h3 class="ui-section-title" id="' + idPrefix + '-hist">History</h3>' + personDlHtml(history) + '</section>' : '');
}

function personIdentityHtml(model, idPrefix) {
    var rows = [['LXMF address', RS.kit.hash.copy(model.hash, { form: 'full', noun: 'LXMF address' })]];
    rows.push(model.identityHash
        ? ['Identity hash', RS.kit.hash.copy(model.identityHash, { form: 'full', noun: 'Identity hash' })]
        : ['Identity hash', 'Not known yet. It shows after they announce.', { muted: true }]);
    return '<section aria-labelledby="' + idPrefix + '-id"><h3 class="ui-section-title" id="' + idPrefix + '-id">Identity</h3>' + personDlHtml(rows, true) + '</section>';
}

function personRelationHtml(model) {
    return '<div class="person-danger">' +
        (model.isContact
            ? '<button type="button" class="ui-menu-item" data-person-action="remove">' + RS.kit.icon('user-minus') + '<span class="lbl">Remove from contacts</span></button>'
            : '<button type="button" class="ui-menu-item" data-person-action="add">' + RS.kit.icon('user-plus') + '<span class="lbl">Add to contacts</span></button>') +
        '<button type="button" class="ui-menu-item ui-menu-item--danger" data-person-action="block">' + RS.kit.icon('ban') + '<span class="lbl">Block</span></button>' +
    '</div>';
}

// Call shows only where calling works; it follows the call state (answer, hang up).
function personCallButtonHtml(id, hash, large) {
    var action = typeof _voiceActionState === 'function' ? _voiceActionState(hash) : { available: false };
    if (!action.available) return '';
    var label = action.label === 'Call in Progress' ? 'Call in progress' : action.label;
    return '<button type="button" class="ui-btn ui-btn--pair' + (large ? ' ui-btn--lg' : '') + (action.danger ? ' ui-btn--danger' : '') +
        '" id="' + id + '" data-hash="' + escapeHtml(hash) + '"' + (action.disabled ? ' disabled' : '') + '>' + RS.kit.icon('phone') + escapeHtml(label) + '</button>';
}

function personActionsHtml(model, idPrefix, large) {
    return '<div class="person-actions">' +
        '<button type="button" class="ui-btn ui-btn--primary ui-btn--pair' + (large ? ' ui-btn--lg' : '') + '" id="' + idPrefix + '-message-btn">' + RS.kit.icon('message') + 'Message</button>' +
        personCallButtonHtml(idPrefix + '-call-btn', model.hash, large) +
    '</div>';
}

// kind: 'peer' | 'contact'. tag: the heading element for a dialog title.
function personNameRowHtml(model, kind, id, inDialog) {
    var cls = inDialog ? 'ui-dialog-title' : 'ui-detail-title';
    if (!model.name) {
        return '<div class="person-name-row"><h2 class="' + cls + ' is-placeholder" id="' + id + '">' + (kind === 'contact' ? 'Unnamed contact' : 'Unnamed peer') + '</h2>' +
            (kind === 'contact' ? '<button type="button" class="ui-link" data-person-action="rename">' + RS.kit.icon('edit') + 'Add name</button>' : '') + '</div>';
    }
    return '<div class="person-name-row"><h2 class="' + cls + '" id="' + id + '"><bdi dir="auto">' + escapeHtml(model.name) + '</bdi></h2>' +
        (model.ratspeak && kind === 'peer' ? RS.kit.mark() : '') +
        (kind === 'contact' ? '<button type="button" class="ui-icon-btn ui-icon-btn--sm" aria-label="Rename" title="Rename" data-person-action="rename">' + RS.kit.icon('edit') + '</button>' : '') + '</div>';
}

function personStatusHtml(model) {
    var peer = model.peer;
    var same = peer && peer.path_updated && model.lastSeen && Math.abs(peer.path_updated - model.lastSeen) <= 60;
    return '<p class="person-status"' + (same ? ' title="Path updated at the same time"' : '') + '>' + escapeHtml(personHeardLine(model)) + '</p>' +
        (model.status ? '<p class="person-quote">“' + escapeHtml(model.status) + '”</p>' : '');
}

function personAvatarHtml(model, size) {
    var tier = RS.kit.presenceFor(model.lastSeen);
    return RS.kit.avatar(model.hash, size, tier, model.lastSeen);
}

// The desktop detail for a peer or a contact (one component for both).
function personDetailHtml(model, kind, idPrefix) {
    return '<header class="person-head"><div class="person-wrap">' +
            personAvatarHtml(model, 64) +
            '<div class="person-id">' + personNameRowHtml(model, kind, idPrefix + '-name', false) + personStatusHtml(model) + '</div>' +
            personActionsHtml(model, idPrefix, false) +
        '</div></header>' +
        '<div class="person-wrap person-body">' +
            '<div class="person-main">' + personFactsHtml(model, idPrefix) + '</div>' +
            '<aside class="person-side" aria-label="Identity and ' + kind + ' actions">' + personIdentityHtml(model, idPrefix) + personRelationHtml(model) + '</aside>' +
        '</div>';
}

// Message, Call, and the relationship actions. opts: { before, onBlocked, onRename }
function wirePersonActions(root, model, idPrefix, opts) {
    opts = opts || {};
    var hash = model.hash;
    var label = model.name || shortHash(hash, 8, 4);
    var message = root.querySelector('#' + idPrefix + '-message-btn');
    if (message) message.addEventListener('click', function() {
        if (typeof opts.before === 'function') opts.before();
        if (typeof openConversationWith === 'function') openConversationWith(hash);
    });
    if (typeof wireVoiceActionButton === 'function' && root.querySelector('#' + idPrefix + '-call-btn')) {
        wireVoiceActionButton(idPrefix + '-call-btn', opts.before);
    }
    root.querySelectorAll('[data-person-action]').forEach(function(btn) {
        btn.addEventListener('click', function() {
            var action = btn.getAttribute('data-person-action');
            if (action === 'rename') {
                if (typeof opts.onRename === 'function') opts.onRename();
                else openRenameContactDialog(hash, model.name);
                return;
            }
            if (typeof opts.before === 'function') opts.before();
            if (action === 'add') openAddContactDialog({ hash: hash });
            else if (action === 'remove') peerConfirmRemoveContact(hash, label);
            else if (action === 'block') peerConfirmBlock(hash, label, opts.onBlocked);
        });
    });
}

// The phone person sheet: the detail header, Message and Call, the facts folded under Details, then the relationship actions.
var _personSheet = null;
function openPersonSheet(hash, kind) {
    closePersonSheet();
    var model = personModel(hash);
    var idPrefix = 'person-sheet';
    var head = '<div class="person-sheet-head">' + personAvatarHtml(model, 64) +
        '<div class="person-id">' + personNameRowHtml(model, kind, idPrefix + '-name', true) + personStatusHtml(model) + '</div></div>';
    var facts = '<div class="person-details">' + personFactsHtml(model, idPrefix) + personIdentityHtml(model, idPrefix) + '</div>';
    var built = RS.kit.dialog({
        size: 'md',
        className: 'person-sheet',
        head: head,
        body: personActionsHtml(model, idPrefix, true) + peerDisclosureHtml(idPrefix + '-details', facts, false) + personRelationHtml(model),
        onClose: function() { if (_personSheet === built) _personSheet = null; }
    });
    _personSheet = built;
    built.hash = hash;
    built.kind = kind;
    wirePersonActions(built.dialog, model, idPrefix, {
        before: function() { built.close(null); },
        onRename: function() { openRenameContactDialog(hash, model.name); }
    });
    return built;
}

function closePersonSheet() {
    if (_personSheet) _personSheet.close(null);
}

// Rename a saved contact: the name only you see. add_contact is upsert-by-hash.
function openRenameContactDialog(hash, currentName) {
    var built = RS.kit.dialog({
        title: currentName ? 'Rename contact' : 'Add a name',
        body: '<div class="ui-field"><label class="ui-label" for="contact-rename-input">Name</label>' +
            '<input class="ui-input" id="contact-rename-input" maxlength="64" autocomplete="off" value="' + escapeHtml(currentName || '') + '" aria-describedby="contact-rename-help">' +
            '<p class="ui-help" id="contact-rename-help">Only you see this name. Leave it empty to use the one they announce.</p></div>',
        foot: '<button type="button" class="ui-btn ui-btn--quiet ui-desk-only" data-ui-close>Cancel</button><button type="button" class="ui-btn ui-btn--primary" id="contact-rename-save">Save</button>'
    });
    var input = built.q('#contact-rename-input');
    if (typeof disableAutoCorrect === 'function') disableAutoCorrect(input);
    function save() {
        var name = input.value.trim();
        built.close(name);
        RS.invokeOrToast('add_contact', { args: { hash: hash, display_name: name || null } }, 'Could not rename the contact');
    }
    built.q('#contact-rename-save').addEventListener('click', save);
    input.addEventListener('keydown', function(e) { if (e.key === 'Enter') { e.preventDefault(); save(); } });
    if (!RS.kit.compact()) input.select();
    return built;
}

// ---------------------------------------------------------------- legacy helpers (About sheet, Network sheet)

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

// Green: heard recently. Red: never heard and no path. Orange: everything else.
function peerPresenceTone(peer) {
    var status = peer ? peer.status : 'unknown';
    if (status === 'reachable' || status === 'direct') return 'online';
    if (!peer || (!peer.last_seen && !peer.in_path)) return 'never';
    return 'away';
}

// The avatar with its presence dot; className keeps each panel's own size and spacing.
function peerAvatarHtml(className, hash, size, peer) {
    var tone = peerPresenceTone(peer);
    var label = tone === 'never' ? 'Not heard yet' : ((peer && peer.activity_label) || 'Not heard yet');
    return '<div class="' + className + ' peer-avatar">' +
        (typeof identityAvatar === 'function' ? identityAvatar(hash, size) : '') +
        '<span class="peer-presence peer-presence--' + tone + '" role="img" aria-label="' + escapeHtml(label) +
            '" title="' + escapeHtml(label) + '"></span>' +
    '</div>';
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
        html += _peerDetailRowHtml('Connection',
            escapeHtml(personConnectionName(peer.iface) + (peer.iface_is_live ? '' : ' \u00b7 last known')));
    }
    if (inPath && peer.path_updated) html += _peerTimeRowHtml('Path updated', peer.path_updated);
    html += peer && peer.last_seen
        ? _peerTimeRowHtml('Last heard', peer.last_seen)
        : _peerDetailRowHtml('Last heard', 'Not heard yet', { muted: true });
    if (peer && peer.first_seen) html += _peerTimeRowHtml('First heard', peer.first_seen);
    if (opts.trust) html += _peerDetailRowHtml('Trust', escapeHtml(opts.trust));

    return html + '</div>';
}

// The Details disclosure around the technical rows. The markup is always closed;
// syncPeerDetails applies the remembered state once it is in a container.
function peerDetailsHtml(id, peer, opts) {
    return peerDisclosureHtml(id, peerDetailRowsHtml(peer, opts), true);
}

// The Details disclosure around any content. Unremembered, it opens closed every time and toggles alone.
function peerDisclosureHtml(id, contentHtml, remember) {
    return '<div class="peer-details" data-open="false"' + (remember ? '' : ' data-remember="false"') + '>' +
        '<button type="button" class="peer-details-toggle" aria-expanded="false" aria-controls="' + id + '">' +
            '<span>Details</span>' + PEER_ICON_CHEVRON +
        '</button>' +
        '<div class="peer-details-region" id="' + id + '" role="region" aria-label="Details" inert>' +
            contentHtml +
        '</div>' +
    '</div>';
}

function _setPeerDetailsOpen(box, open) {
    box.setAttribute('data-open', open);
    box.querySelector('.peer-details-toggle').setAttribute('aria-expanded', open);
    // Closed rows stay in the DOM for the slide; inert keeps them out of the tab order.
    box.querySelector('.peer-details-region').inert = !open;
}

function syncPeerDetails(root) {
    root.querySelectorAll('.peer-details:not([data-remember="false"])').forEach(function(box) {
        _setPeerDetailsOpen(box, _peerDetailsOpen);
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

function peerPromptAddContact(hash) {
    openAddContactDialog({ hash: hash });
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
    var own = e.target.closest('.peer-details[data-remember="false"]');
    if (own) { _setPeerDetailsOpen(own, own.getAttribute('data-open') !== 'true'); return; }
    _peerDetailsOpen = !_peerDetailsOpen;
    try { localStorage.setItem(PEER_DETAILS_OPEN_KEY, _peerDetailsOpen ? '1' : '0'); } catch (err) {}
    syncPeerDetails(document);
});
