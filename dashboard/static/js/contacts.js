// Contacts: the saved people list with letter captions, the detail beside it on
// desktop (the first contact is selected), the person sheet on a phone, and the
// Add contact flow (by LXMF address, or from a QR code).

var contactsSelectedHash = null;
var _contactsLoadFailed = false;
var _contactsLoading = false;

var LXMF_ADDRESS_LENGTH = 32;

// A–Z, then "#" for names starting with anything else, then contacts with no name.
function contactGroupKey(c) {
    var name = ((c && c.display_name) || '').trim();
    if (!name || name === c.hash) return 'Unnamed';
    var ch = name.charAt(0);
    try { ch = ch.normalize('NFD').charAt(0); } catch (_) {}
    return /^[A-Za-z]$/.test(ch) ? ch.toUpperCase() : '#';
}

function _contactGroupRank(key) { return key === 'Unnamed' ? 2 : key === '#' ? 1 : 0; }

function contactsGrouped(list) {
    var sorted = list.slice().sort(function(a, b) {
        var ga = contactGroupKey(a), gb = contactGroupKey(b);
        if (_contactGroupRank(ga) !== _contactGroupRank(gb)) return _contactGroupRank(ga) - _contactGroupRank(gb);
        return (a.display_name || a.hash).localeCompare(b.display_name || b.hash, undefined, { sensitivity: 'base' });
    });
    var groups = [];
    sorted.forEach(function(c) {
        var key = contactGroupKey(c);
        if (!groups.length || groups[groups.length - 1].label !== key) groups.push({ label: key, items: [] });
        groups[groups.length - 1].items.push(c);
    });
    return groups;
}

function contactMatches(c, query) {
    if (!query) return true;
    return (c.display_name || '').toLowerCase().indexOf(query) !== -1 || c.hash.toLowerCase().indexOf(query) !== -1;
}

function _contactsQuery() {
    var input = document.getElementById('contacts-search');
    return input ? input.value.toLowerCase().trim() : '';
}

function _contactRowHtml(c, current) {
    var model = personModel(c.hash);
    var unnamed = !model.name;
    var title = unnamed
        ? '<span class="ui-sr">Unnamed contact, address </span>' + escapeHtml(RS.kit.hash.short(c.hash))
        : '<bdi dir="auto">' + escapeHtml(model.name) + '</bdi>';
    var time = model.lastSeen
        ? '<span class="ui-time" title="' + escapeHtml(RS.kit.time.full(model.lastSeen)) + '"><span aria-hidden="true">' + escapeHtml(RS.kit.time.row(model.lastSeen)) +
            '</span><span class="ui-sr">, last heard ' + escapeHtml(RS.kit.time.sentence(model.lastSeen)) + '</span></span>'
        : '';
    return '<li><button type="button" class="ui-row' + (unnamed ? ' is-unnamed' : '') + '" data-hash="' + escapeHtml(c.hash) + '"' + (current ? ' aria-current="true"' : '') + '>' +
        RS.kit.avatar(c.hash, 'row', RS.kit.rowPresence(model.lastSeen), model.lastSeen) +
        '<span class="ui-row-main"><span class="ui-row-top"><span class="ui-row-title">' + title + '</span>' + time + '</span>' +
            (unnamed ? '' : '<span class="ui-row-sub"><span class="t ui-hash" aria-hidden="true">' + escapeHtml(RS.kit.hash.short(c.hash)) + '</span></span>') +
        '</span></button></li>';
}

function _contactsMyAddress() {
    var active = typeof activeIdentity === 'function' ? activeIdentity() : null;
    return (active && active.lxmf_hash) || '';
}

function contactsFirstRunHtml() {
    var address = _contactsMyAddress();
    return '<div class="person-onboard"><span class="ui-tile ui-tile--lg" aria-hidden="true">' + RS.kit.icon('users') + '</span>' +
        '<h3 class="ui-empty-title">Add your first contact</h3>' +
        '<p class="ui-empty-body">Save the people you talk to so they’re easy to find, message, and call.</p>' +
        '<div class="ui-action-tiles">' +
            '<button type="button" class="ui-action-tile" data-contacts-add="address"><span class="ui-tile" aria-hidden="true">' + RS.kit.icon('hash') + '</span><span class="ui-action-tile-text"><span class="ui-action-tile-title">By address</span><span class="ui-action-tile-hint">Paste the address they shared with you.</span></span></button>' +
            '<button type="button" class="ui-action-tile" data-contacts-add="qr"><span class="ui-tile" aria-hidden="true">' + RS.kit.icon('qr') + '</span><span class="ui-action-tile-text"><span class="ui-action-tile-title">From QR code</span><span class="ui-action-tile-hint">Scan it, choose an image, or paste it.</span></span></button>' +
        '</div>' +
        (address
            ? '<div class="person-share"><div class="person-share-text"><h4 class="ui-card-title">Your address</h4><p class="ui-empty-body">Share it so others can add you.</p></div>' +
                '<div class="person-share-actions">' + RS.kit.hash.copy(address, { form: 'fit', noun: 'LXMF address' }) +
                '<button type="button" class="ui-btn ui-btn--sm" data-contacts-add="card">' + RS.kit.icon('qr') + 'Show QR code</button></div></div>'
            : '') +
    '</div>';
}

function _contactsErrorHtml() {
    return '<div class="ui-empty"><span class="ui-tile ui-tile--lg ui-tile--error" aria-hidden="true">' + RS.kit.icon('alert') + '</span>' +
        '<h3 class="ui-empty-title">Contacts didn’t load</h3><p class="ui-empty-body">Ratspeak couldn’t read your saved contacts. Nothing was removed.</p>' +
        '<div class="ui-empty-actions"><button type="button" class="ui-btn" data-contacts-retry>' + RS.kit.icon('refresh') + 'Try again</button></div></div>';
}

function renderStandaloneContactList() {
    var container = document.getElementById('contacts-standalone-list');
    var detail = document.getElementById('contacts-detail');
    if (!container) return;
    if (!container._ptrAttached && RS.gestures && RS.gestures.attachPullToRefresh) {
        RS.gestures.attachPullToRefresh(container, { onRefresh: renderStandaloneContactList });
    }
    if (_gateHidden('view-contacts', container)) return;

    var all = Array.isArray(lxmfContacts) ? lxmfContacts : [];
    var phone = isCompactLayout();
    var query = _contactsQuery();
    var countEl = document.getElementById('contacts-count');
    if (countEl) countEl.innerHTML = all.length ? '<span class="ui-sr">, </span>' + all.length : '';
    var clear = document.getElementById('contacts-search-clear');
    if (clear) clear.hidden = !query;

    var hits = all.filter(function(c) { return contactMatches(c, query); });
    var groups = contactsGrouped(hits);
    var state = !all.length ? (_contactsLoadFailed ? 'error' : 'empty') : (!hits.length ? 'none' : 'list');

    if (state === 'list' && !phone) {
        var stillThere = contactsSelectedHash && hits.some(function(c) { return c.hash === contactsSelectedHash; });
        var known = contactsSelectedHash && all.some(function(c) { return c.hash === contactsSelectedHash; });
        // C4: restore the last selection, or select the first contact; a search that hides it clears the detail.
        if (!known) contactsSelectedHash = groups[0].items[0].hash;
        else if (!stillThere && !query) contactsSelectedHash = groups[0].items[0].hash;
    }

    var html;
    if (state === 'error') html = _contactsErrorHtml();
    else if (state === 'empty') html = phone ? contactsFirstRunHtml() : '<div class="ui-empty ui-empty--quiet"><p class="ui-empty-body">Contacts you add show here.</p></div>';
    else if (state === 'none') {
        html = '<div class="ui-empty"><h3 class="ui-empty-title">No contacts match “' + escapeHtml(query) + '”</h3><p class="ui-empty-body">Search checks names and addresses.</p>' +
            '<div class="ui-empty-actions"><button type="button" class="ui-link" data-contacts-clear>Clear search</button></div></div>';
    } else {
        html = groups.map(function(g, index) {
            var id = 'contacts-group-' + index;
            return '<div class="ui-list" role="group" aria-labelledby="' + id + '"><h3 class="ui-caption" id="' + id + '">' + escapeHtml(g.label) + '</h3>' +
                '<ul class="person-rows">' + g.items.map(function(c) { return _contactRowHtml(c, !phone && c.hash === contactsSelectedHash); }).join('') + '</ul></div>';
        }).join('');
    }
    if (!_gateClean(container, html)) container.innerHTML = html;
    if (detail && !phone) renderContactDetail(state, hits);
}

function renderContactDetail(state, hits) {
    var detail = document.getElementById('contacts-detail');
    if (!detail) return;
    var html, model = null;
    if (state === 'empty') html = contactsFirstRunHtml();
    else if (state === 'error') html = '';
    else {
        var visible = contactsSelectedHash && (hits || []).some(function(c) { return c.hash === contactsSelectedHash; });
        if (visible) {
            model = personModel(contactsSelectedHash);
            html = personDetailHtml(model, 'contact', 'contact-detail');
        } else {
            html = '<div class="ui-empty ui-empty--quiet ui-empty--center"><h3 class="ui-empty-title">Select a contact</h3><p class="ui-empty-body">Their address, route, and actions show here.</p></div>';
        }
    }
    // The detail re-renders with the list; unchanged markup stays so focus and hover survive.
    if (detail._rsLastHtml === html) return;
    detail._rsLastHtml = html;
    detail.innerHTML = html;
    if (model) wirePersonActions(detail, model, 'contact-detail', { onBlocked: function() { contactsSelectedHash = null; } });
}

function selectContact(hash) {
    if (isCompactLayout()) {
        openPersonSheet(hash, 'contact');
        return;
    }
    contactsSelectedHash = hash;
    var container = document.getElementById('contacts-standalone-list');
    if (container) container._rsLastHtml = null;
    renderStandaloneContactList();
}

// Opens a saved contact: the person sheet anywhere, as Home and chats ask for it.
function showContactDetailSheet(hash) {
    openPersonSheet(hash, 'contact');
}

// Opens Contacts on this contact (desktop selects it; a phone opens its sheet).
function openContactInContacts(hash) {
    contactsSelectedHash = hash;
    if (typeof switchView === 'function') switchView('contacts');
    if (isCompactLayout()) openPersonSheet(hash, 'contact');
    else renderStandaloneContactList();
}

function contactsViewActivated() {
    renderStandaloneContactList();
    // Re-fetch when the contacts_update event was missed; a failure shows the error state.
    if (Array.isArray(lxmfContacts) && lxmfContacts.length) return;
    if (_contactsLoading || typeof RS === 'undefined' || typeof RS.invoke !== 'function') return;
    _contactsLoading = true;
    var owner = RS.conversationOwner ? RS.conversationOwner.snapshot() : null;
    var before = lxmfContacts;
    RS.invoke('api_contacts').then(function(data) {
        _contactsLoading = false;
        _contactsLoadFailed = false;
        if (owner && !RS.conversationOwner.isIdentityCurrent(owner)) return;
        if (lxmfContacts !== before) return;
        if (Array.isArray(data)) lxmfContacts = normalizeContactList(data);
        renderStandaloneContactList();
    }).catch(function() {
        _contactsLoading = false;
        _contactsLoadFailed = !(Array.isArray(lxmfContacts) && lxmfContacts.length);
        renderStandaloneContactList();
    });
}

// ---------------------------------------------------------------- Add contact

function openContactAddOptions(trigger) {
    var items = [
        { label: 'By address', hint: 'Paste the address they shared with you.', icon: 'hash', onSelect: function() { openAddContactDialog(); } },
        { label: 'From QR code', hint: 'Scan it, choose an image, or paste it.', icon: 'qr', onSelect: function() { if (typeof openContactQrScanner === 'function') openContactQrScanner(); } }
    ];
    if (!trigger) { openAddContactDialog(); return; }
    RS.kit.menu(trigger, { title: 'Add contact', items: items });
}

// An LXMF address from what was typed or pasted: spaces, brackets, and an lxma:// prefix drop away.
function normalizeLxmfAddressInput(text) {
    var value = String(text || '').trim();
    var link = /^lxma:\/\/([0-9a-f]+)/i.exec(value);
    if (link) value = link[1];
    return value.replace(/[\s<>]/g, '').toLowerCase();
}

function isLxmfAddress(value) {
    return value.length === LXMF_ADDRESS_LENGTH && /^[0-9a-f]+$/.test(value);
}

function _addContactMetaText(model) {
    var parts = [personHeardLine(model)];
    var peer = model.peer;
    var kind = peer && peer.iface_is_live ? personConnectionKind(peer.iface) : '';
    if (kind) parts.push(kind);
    if (model.ratspeak) parts.push('Ratspeak');
    return parts.join(' · ');
}

// opts: { hash, name }
function openAddContactDialog(opts) {
    opts = opts || {};
    var built = RS.kit.dialog({
        title: 'Add contact',
        className: 'add-contact',
        body: '<div class="ui-field"><label class="ui-label" for="add-contact-address">LXMF address</label>' +
                '<input class="ui-input ui-input--mono" id="add-contact-address" placeholder="32 characters" autocomplete="off" spellcheck="false" autocapitalize="none" aria-describedby="add-contact-address-help">' +
                '<p class="ui-help" id="add-contact-address-help">The address they sent you, or the one under their name in Ratspeak.</p>' +
                '<p class="ui-error" id="add-contact-address-error" hidden>' + RS.kit.icon('alert') + '<span></span></p></div>' +
            '<div class="ui-notice ui-notice--info" id="add-contact-identity" hidden>' + RS.kit.icon('info') + '<div class="ui-notice-body"></div></div>' +
            '<div class="ui-well add-contact-known" id="add-contact-known" hidden></div>' +
            '<div class="ui-notice ui-notice--info" id="add-contact-exists" hidden>' + RS.kit.icon('info') + '<div class="ui-notice-body"></div></div>' +
            '<div class="ui-field" id="add-contact-name-field"><label class="ui-label" for="add-contact-name">Name</label>' +
                '<input class="ui-input" id="add-contact-name" maxlength="64" placeholder="Optional" autocomplete="off" aria-describedby="add-contact-name-help">' +
                '<p class="ui-help" id="add-contact-name-help">Only you see this name.</p></div>',
        foot: '<button type="button" class="ui-btn ui-btn--quiet ui-desk-only" data-ui-close>Cancel</button>' +
            '<button type="button" class="ui-btn ui-btn--primary" id="add-contact-submit">Add contact</button>',
        focus: '#add-contact-address'
    });
    var address = built.q('#add-contact-address');
    var name = built.q('#add-contact-name');
    var nameField = built.q('#add-contact-name-field');
    var nameHelp = built.q('#add-contact-name-help');
    var error = built.q('#add-contact-address-error');
    var help = built.q('#add-contact-address-help');
    var knownWell = built.q('#add-contact-known');
    var exists = built.q('#add-contact-exists');
    var fromIdentity = built.q('#add-contact-identity');
    var submit = built.q('#add-contact-submit');
    if (typeof disableAutoCorrect === 'function') { disableAutoCorrect(address); disableAutoCorrect(name); }
    address.value = opts.hash || '';
    name.value = opts.name || '';
    var existing = null;

    function showError(text) {
        error.querySelector('span').textContent = text;
        error.hidden = !text;
        help.hidden = !!text;
        address.classList.toggle('is-invalid', !!text);
        if (text) {
            address.setAttribute('aria-invalid', 'true');
            address.setAttribute('aria-describedby', 'add-contact-address-error');
        } else {
            address.removeAttribute('aria-invalid');
            address.setAttribute('aria-describedby', 'add-contact-address-help');
        }
    }

    function evaluate(strict) {
        var entered = normalizeLxmfAddressInput(address.value);
        var valid = isLxmfAddress(entered);
        // Their identity hash, if heard, stands for their LXMF address.
        var resolved = valid ? resolvedLxmfAddress(entered) : null;
        var value = resolved && resolved.fromIdentity ? resolved.address : entered;
        if (valid && !resolved && typeof resolveLxmfAddress === 'function') {
            resolveLxmfAddress(entered).then(function(answer) {
                if (answer.fromIdentity && normalizeLxmfAddressInput(address.value) === entered) evaluate(false);
            });
        }
        fromIdentity.hidden = value === entered;
        if (value !== entered) {
            fromIdentity.querySelector('.ui-notice-body').innerHTML = 'That’s their identity hash. Using their LXMF address, <span class="ui-hash">' +
                escapeHtml(RS.kit.hash.short(value)) + '</span>.';
        }
        existing = null;
        var model = valid ? personModel(value) : null;
        if (model && model.contact) existing = model;
        if (strict && entered && !valid) {
            showError('An LXMF address is 32 characters, using 0–9 and a–f. This one has ' + entered.length + '.');
        } else if (!strict || valid) {
            showError('');
        }
        exists.hidden = !existing;
        nameField.hidden = !!existing;
        if (existing) {
            exists.querySelector('.ui-notice-body').innerHTML = '<b>' + escapeHtml(existing.name || RS.kit.hash.short(value)) + '</b> is already in your contacts.';
        }
        submit.textContent = existing ? 'Open contact' : 'Add contact';
        var known = model && !existing && model.peer && model.peer.display_name ? model : null;
        knownWell.hidden = !known;
        if (known) {
            knownWell.innerHTML = RS.kit.avatar(known.hash, 40) + '<div class="add-contact-known-text"><span class="ui-card-title">' + escapeHtml(known.peer.display_name) + '</span>' +
                '<span class="person-status">' + escapeHtml(_addContactMetaText(known)) + '</span></div>';
        }
        name.placeholder = known ? known.peer.display_name : 'Optional';
        nameHelp.textContent = 'Only you see this name.' + (known ? ' Leave it empty to use the one they announce.' : '');
        return valid ? value : null;
    }

    function confirm() {
        var value = evaluate(true);
        if (!value) {
            if (!address.value.trim()) showError('Enter their LXMF address. It is 32 characters, using 0–9 and a–f.');
            address.focus();
            return;
        }
        if (existing) {
            built.close(value);
            openContactInContacts(value);
            return;
        }
        submit.disabled = true;
        RS.invoke('add_contact', { args: { hash: value, display_name: name.value.trim() || null } }).then(function(result) {
            var saved = (result && result.hash) || value;
            built.close(saved);
            if (typeof showToast === 'function') showToast('Contact added', 'toast-success', 2500);
            if (!isCompactLayout() && isViewActive('contacts')) selectContact(saved);
        }).catch(function(err) {
            submit.disabled = false;
            if (err && err.code === 'locked') return;
            if (typeof showToast === 'function') showToast(typeof toastErrorCopy === 'function' ? toastErrorCopy(err, 'Could not add the contact') : 'Could not add the contact', 'toast-error', 3500);
        });
    }

    address.addEventListener('input', function() { evaluate(false); });
    address.addEventListener('blur', function() { if (address.value.trim()) evaluate(true); });
    address.addEventListener('keydown', function(e) { if (e.key === 'Enter') { e.preventDefault(); confirm(); } });
    name.addEventListener('keydown', function(e) { if (e.key === 'Enter') { e.preventDefault(); confirm(); } });
    submit.addEventListener('click', confirm);
    evaluate(!!opts.hash);
    if (opts.hash && !RS.kit.compact()) name.focus();
    return built;
}

document.addEventListener('DOMContentLoaded', function() {
    var view = document.getElementById('view-contacts');
    var search = document.getElementById('contacts-search');
    var clear = document.getElementById('contacts-search-clear');
    var addBtn = document.getElementById('contacts-add-btn');
    if (search) {
        if (typeof disableAutoCorrect === 'function') disableAutoCorrect(search);
        search.addEventListener('input', renderStandaloneContactList);
        search.addEventListener('keydown', function(e) {
            if (e.key === 'Enter') { e.preventDefault(); this.blur(); }
            if (e.key === 'Escape' && this.value) { e.preventDefault(); this.value = ''; renderStandaloneContactList(); }
        });
    }
    if (clear) clear.addEventListener('click', function() {
        search.value = '';
        renderStandaloneContactList();
        search.focus();
    });
    if (addBtn) addBtn.addEventListener('click', function() { openContactAddOptions(addBtn); });
    if (!view) return;
    view.addEventListener('click', function(e) {
        var row = e.target.closest('.ui-row[data-hash]');
        if (row) { selectContact(row.getAttribute('data-hash')); return; }
        var add = e.target.closest('[data-contacts-add]');
        if (add) {
            var kind = add.getAttribute('data-contacts-add');
            if (kind === 'address') openAddContactDialog();
            else if (kind === 'qr' && typeof openContactQrScanner === 'function') openContactQrScanner();
            else if (kind === 'card' && typeof openIdentityShareScreen === 'function') openIdentityShareScreen();
            return;
        }
        if (e.target.closest('[data-contacts-clear]')) {
            search.value = '';
            renderStandaloneContactList();
            return;
        }
        if (e.target.closest('[data-contacts-retry]')) contactsViewActivated();
    });
    if (typeof watchViewLayout === 'function') {
        watchViewLayout('contacts', function() {
            var list = document.getElementById('contacts-standalone-list');
            var detail = document.getElementById('contacts-detail');
            if (list) list._rsLastHtml = null;
            if (detail) detail._rsLastHtml = null;
            renderStandaloneContactList();
        });
    }
});

// The detail's heard times and routes follow the peer cache while Contacts is on screen.
if (typeof PeersCache !== 'undefined' && PeersCache && typeof PeersCache.subscribe === 'function') {
    PeersCache.subscribe(function() {
        if (!isViewActive('contacts') || document.hidden) return;
        renderStandaloneContactList();
    });
}

window.openAddContactDialog = openAddContactDialog;
window.openContactAddOptions = openContactAddOptions;
window.showContactDetailSheet = showContactDetailSheet;
window.openContactInContacts = openContactInContacts;
