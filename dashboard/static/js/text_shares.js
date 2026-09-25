// Android system-share intake. Content stays literal and Send stays explicit.
(function() {
    'use strict';
    if (typeof isAndroid !== 'function' || !isAndroid()) return;
    var sheet = document.getElementById('text-share-sheet');
    var overlay = document.getElementById('text-share-overlay');
    var pendingButton = document.getElementById('text-share-pending');
    if (!sheet || !overlay || !pendingButton) return;
    var list = document.getElementById('text-share-list');
    var preview = document.getElementById('text-share-preview');
    var title = document.getElementById('text-share-title');
    var search = document.getElementById('text-share-search');
    var contacts = document.getElementById('text-share-contacts');
    var back = document.getElementById('text-share-back');
    var confirm = document.getElementById('text-share-confirm');
    var sender = document.getElementById('text-share-sender');
    var input = document.getElementById('lxmf-input');
    var items = [], selected = null, stamp = null, epoch = 0, mode = 'recent';
    var loading = false, again = false, needsRefresh = true, offered = Object.create(null);
    var refreshRevision = 0, mutationRevision = 0, editChain = Promise.resolve();
    var drafts = Object.create(null), saveTimer = null, selecting = false;
    var recipient = null, reviewing = false;

    function visible() {
        return document.visibilityState === 'visible' && !window._identitySwitchInProgress &&
            !(typeof _isSetupActive === 'function' && _isSetupActive());
    }
    function mediaBusy() {
        return !!lxmfPendingFile || !!_replyTarget ||
            !!(RS.voiceMemos && RS.voiceMemos.hasPendingRecording && RS.voiceMemos.hasPendingRecording());
    }
    function sharedMediaBusy(holder) {
        return !!_replyTarget || !!(RS.voiceMemos && RS.voiceMemos.hasPendingRecording && RS.voiceMemos.hasPendingRecording()) ||
            (holder.item.image ? !holder.attachment || lxmfPendingFile !== holder.attachment || holder.attachment.preparing
                : !!lxmfPendingFile);
    }
    function error(message) { showToast(String(message && message.message || message), 'toast-warning', 4200); }
    function current(owner, session) {
        return session === epoch && visible() && RS.conversationOwner.isIdentityCurrent(owner);
    }
    function exact(owner, session) { return current(owner, session) && RS.conversationOwner.isCurrent(owner); }
    function updateButton() {
        pendingButton.hidden = items.length === 0 && !needsRefresh;
        pendingButton.textContent = items.length ? 'Shared items (' + items.length + ')' : 'Shared items';
    }
    function close() {
        epoch += 1;
        selected = null;
        recipient = null;
        selecting = false;
        confirm.disabled = true;
        search.value = '';
        preview.textContent = '';
        list.replaceChildren();
        RS.ui.closeExistingSheet(sheet, overlay);
    }
    function reset() {
        clearTimeout(saveTimer);
        close();
        drafts = Object.create(null);
        items = []; stamp = null; needsRefresh = true; offered = Object.create(null);
        refreshRevision += 1;
        pendingButton.hidden = true;
    }
    function put(item, id) {
        items = items.filter(function(old) { return old.id !== id; });
        if (item) items.push(item);
        updateButton();
    }
    function edit(holder, operation, extra) {
        var identityOwner = RS.conversationOwner.snapshot();
        var capturedStamp = stamp && Object.assign({}, stamp);
        var task = editChain.catch(function() {}).then(function() {
            if (!capturedStamp || !visible() || !RS.conversationOwner.isIdentityCurrent(identityOwner)) {
                throw new Error('The identity changed. Open the shared draft again.');
            }
            mutationRevision += 1;
            return RS.invoke('edit_text_share', { args: Object.assign({
                id: holder.item.id, revision: holder.item.revision,
                activity_generation: capturedStamp.activity_generation,
                identity_generation: capturedStamp.identity_generation,
                operation: operation
            }, extra || {}) }).then(function(result) {
                mutationRevision += 1;
                if (!RS.conversationOwner.isIdentityCurrent(identityOwner)) {
                    if (result.stage) RS.invoke('cancel_attachment_stage', {token: result.stage.token}).catch(function() {});
                    throw new Error('The identity changed.');
                }
                put(result.item, holder.item.id);
                if (result.item) holder.item = result.item;
                return result;
            });
        });
        editChain = task;
        return task;
    }
    function refresh(manual) {
        if (!visible()) return Promise.resolve(false);
        if (loading) { again = true; return Promise.resolve(false); }
        loading = true;
        var request = ++refreshRevision, owner = RS.conversationOwner.snapshot(), revision = mutationRevision;
        return RS.invoke('list_text_shares').then(function(result) {
            if (request !== refreshRevision || !RS.conversationOwner.isIdentityCurrent(owner) || !visible()) return false;
            stamp = result;
            if (revision !== mutationRevision) { again = true; return false; }
            items = result.items || []; needsRefresh = false;
            updateButton();
            if (manual || (items.some(function(item) { return !offered[item.id]; }) && !mediaBusy() &&
                !document.querySelector('.bottom-sheet.open, .modal-overlay.active, [role="dialog"][aria-modal="true"]:not(.bottom-sheet)'))) {
                open(!!manual);
            }
            return true;
        }).catch(function(err) {
            needsRefresh = true;
            if (manual) error(err);
        }).finally(function() {
            loading = false;
            if (again) { again = false; refresh(false); }
        });
    }
    function buttonRow(name, detail, click, hash) {
        var row = document.createElement('button');
        row.type = 'button'; row.className = 'fab-picker-row';
        if (hash && /^[0-9a-f]{32}$/.test(hash)) {
            var avatar = document.createElement('span');
            avatar.className = 'text-share-avatar';
            avatar.innerHTML = identityAvatar(hash, 32);
            row.appendChild(avatar);
        }
        var labels = document.createElement('span'); labels.className = 'text-share-labels';
        var primary = document.createElement('span'); primary.className = 'fab-picker-name'; primary.textContent = name;
        var secondary = document.createElement('span'); secondary.className = 'fab-picker-hash'; secondary.textContent = detail;
        labels.appendChild(primary); labels.appendChild(secondary); row.appendChild(labels);
        row.addEventListener('click', click); return row;
    }
    function status(text) {
        var node = document.createElement('p'); node.className = 'fab-picker-empty'; node.textContent = text; list.appendChild(node);
    }
    function recipients() {
        var rows = mode === 'contacts' ? lxmfContacts.slice() : lxmfConversations.slice();
        var query = search.value.trim().toLocaleLowerCase();
        var seen = Object.create(null);
        rows = rows.filter(function(row) {
            var hash = String(row.hash || '').toLowerCase();
            if (!/^[0-9a-f]{32}$/.test(hash) || seen[hash]) return false;
            seen[hash] = true;
            return !query || ((row.display_name || '') + ' ' + hash).toLocaleLowerCase().indexOf(query) >= 0;
        });
        rows.sort(function(a, b) {
            var order = mode === 'recent' ? (Number(b.timestamp) || 0) - (Number(a.timestamp) || 0)
                : String(a.display_name || '').localeCompare(String(b.display_name || ''));
            return order || a.hash.localeCompare(b.hash);
        });
        return mode === 'recent' ? rows.slice(0, 5) : rows;
    }
    function selectRecipient(row) {
        if (selecting) return;
        if (document.activeElement === search) search.blur();
        recipient = {hash: row.hash.toLowerCase(), display_name: row.display_name};
        render();
        var checked = list.querySelector('[aria-pressed="true"]');
        if (checked) RS.ui.focusAfterUpdate(checked);
    }
    function setMode(next) {
        if (!selected || selecting) return;
        if (document.activeElement === search) search.blur();
        mode = next; recipient = null; search.value = ''; render();
        RS.ui.focusAfterUpdate(next === 'contacts' ? back : contacts);
    }
    function render() {
        var session = epoch;
        list.replaceChildren(); search.hidden = !selected || mode !== 'contacts';
        contacts.hidden = !selected || mode !== 'recent';
        back.hidden = !selected || mode !== 'contacts';
        contacts.disabled = back.disabled = search.disabled = selecting;
        confirm.hidden = !selected;
        confirm.disabled = !selected || !recipient || selecting;
        confirm.textContent = selecting ? 'Opening…' : 'Share';
        document.getElementById('text-share-recent-label').hidden = !selected || mode !== 'recent';
        document.getElementById('text-share-summary').hidden = !!selected && mode === 'contacts';
        preview.hidden = !selected; preview.textContent = selected
            ? (selected.item.image ? 'Photo · ' + selected.item.image.name + (selected.item.text ? '\n' : '') : '') + selected.item.text : '';
        title.textContent = selected ? (mode === 'contacts' ? 'Contacts' : 'Share to…') : 'Shared items';
        sender.textContent = 'From ' + (document.getElementById('msg-profile-name').textContent || 'current identity');
        document.getElementById('text-share-discard').hidden = !selected || !reviewing || mode === 'contacts';
        document.getElementById('text-share-discard').disabled = selecting;
        if (!selected) {
            if (!items.length) status('No shared items. Share text, a link or a photo to Ratspeak from another app.');
            items.forEach(function(item) {
                list.appendChild(buttonRow(item.image ? 'Photo · ' + item.image.name : item.text.slice(0, 100), item.recipient ? 'Saved draft · tap to review' : 'Choose a recipient', function() {
                    if (session !== epoch || !visible()) return;
                    selected = { item: item }; mode = 'recent'; search.value = '';
                    recipient = item.recipient ? {hash: item.recipient, display_name: recipientName(item.recipient)} : null;
                    render(); RS.ui.focusAfterUpdate(confirm.disabled ? contacts : confirm);
                }));
            });
            return;
        }
        var rows = recipients();
        // A saved recipient need not still be a contact or one of the five
        // recent chats. Keep that recovery destination visible before Share.
        if (recipient && !rows.some(function(row) { return row.hash.toLowerCase() === recipient.hash; })) rows.push(recipient);
        if (!rows.length) status(search.value ? 'No matching recipients.' : mode === 'recent'
            ? 'No recent chats. Choose Contacts to start a conversation.' : 'No contacts yet. Add a contact in Ratspeak, then return to Shared items.');
        rows.forEach(function(row) {
            var hash = row.hash.toLowerCase();
            var button = buttonRow(row.display_name || 'Anonymous', shortHash(hash, 8, 4), function() {
                if (session === epoch) selectRecipient(row);
            }, hash);
            button.disabled = selecting;
            button.setAttribute('aria-pressed', String(!!recipient && recipient.hash === hash));
            var check = document.createElement('span'); check.className = 'text-share-check';
            check.setAttribute('aria-hidden', 'true'); check.textContent = '✓'; button.appendChild(check);
            list.appendChild(button);
        });
    }
    function recipientName(hash) {
        var row = lxmfContacts.concat(lxmfConversations).find(function(row) { return String(row.hash || '').toLowerCase() === hash; });
        return row && row.display_name || 'Anonymous';
    }
    function open(review) {
        if (!visible() || !stamp) return;
        close();
        reviewing = review;
        items.forEach(function(item) { offered[item.id] = true; });
        selected = items.length === 1 && !items[0].recipient ? { item: items[0] } : null;
        mode = 'recent'; render();
        RS.ui.openExistingSheet(sheet, overlay);
        var first = list.querySelector('button') || document.getElementById('text-share-close');
        RS.ui.focusAfterUpdate(first);
        // Contacts come from the existing identity-owned snapshot. If bootstrap
        // has not supplied them yet, hydrate without allowing an old reply back.
        if (!lxmfContacts.length) {
            var owner = RS.conversationOwner.snapshot(), prior = lxmfContacts, session = epoch;
            RS.invoke('api_contacts').then(function(rows) {
                if (!current(owner, session) || lxmfContacts !== prior) return;
                lxmfContacts = normalizeContactList(rows || []); render();
            }).catch(function() {});
        }
    }
    async function choose(hash) {
        if (!selected || selecting || !visible()) return;
        if (mediaBusy()) { error('Finish your voice message, attachment or reply first. This share is kept in Shared items.'); return; }
        if (drafts[hash] && drafts[hash].item.id !== selected.item.id) {
            error('Another shared draft is open in this chat. Send it or discard it first.'); return;
        }
        selecting = true;
        render();
        var holder = selected, session = epoch, owner = RS.conversationOwner.snapshot();
        var previous = hash === lxmfActiveContact ? input.value : (_lxmfDrafts[hash] || '');
        var text = holder.item.text;
        try {
            if (holder.item.send_attempted) {
                var retry = await rsChoice({ title: 'Check the conversation first',
                    message: 'Sending was started for this draft. It may already be in the conversation.', choices: [
                        { label: 'View conversation', value: 'view' }, { label: 'Use draft again', value: 'reuse' }
                    ] });
                if (!exact(owner, session) || selected !== holder) return;
                if (retry === 'view') { close(); openConversationWith(hash); return; }
                if (retry !== 'reuse') return;
            }
            if (previous && previous !== text) {
                var choice = await rsChoice({ title: 'This chat has a draft', message: 'Keep your existing text and add the shared content?',
                    choices: [{ label: 'Add to draft', value: 'add' }, { label: 'Keep for later', value: 'later' }] });
                if (!exact(owner, session) || selected !== holder || choice !== 'add') return;
                text = text ? previous + '\n\n' + text : previous;
            }
            if (!exact(owner, session) || mediaBusy()) return;
            await edit(holder, 'assign', { recipient: hash, text: text });
            if (!exact(owner, session) || selected !== holder || mediaBusy()) return;
            var now = hash === lxmfActiveContact ? input.value : (_lxmfDrafts[hash] || '');
            if (now !== previous) { error('The draft changed. Your shared item is saved; open it again.'); return; }
            close();
            openConversationWith(hash, {focusComposer: !holder.item.image});
            input.value = text;
            _lxmfDrafts[hash] = text;
            holder.owner = RS.conversationOwner.snapshot(); holder.sending = false;
            drafts[hash] = holder;
            if (holder.item.image) {
                var imageOwner = holder.owner;
                var nativeStage = function() {
                    return edit(holder, 'stage_image').then(function(result) {
                        var token = result.stage.token;
                        if (!RS.conversationOwner.isCurrent(imageOwner)) {
                            RS.invoke('cancel_attachment_stage', {token: token}).catch(function() {});
                            throw new Error('The conversation changed. Your photo is kept in Shared items.');
                        }
                        return token;
                    });
                };
                holder.attachment = attachSharedImage(holder.item.image, nativeStage);
            }
            input.dispatchEvent(new Event('input'));
            if (!holder.item.image) showToast('Ready to review. Tap Send when you’re ready.', 'toast-success', 2600);
        } catch (err) { if (current(owner, session)) error(err); }
        finally {
            // A retired assignment must not unlock a replacement picker's work.
            if (session === epoch) { selecting = false; render(); }
        }
    }
    function save(holder, text) {
        if (!text.trim() && !holder.item.image) return Promise.resolve(); // Explicit Discard owns deletion.
        return edit(holder, 'draft', { text: text });
    }
    function interceptSend(deliveryMethod) {
        var hash = lxmfActiveContact, holder = drafts[hash];
        if (!holder) return false;
        if (holder.sending) return true;
        if (sharedMediaBusy(holder)) {
            // Removing a shared photo leaves its recovery copy available, but
            // must not trap the composer or turn a replacement into that share.
            if (holder.item.image && lxmfPendingFile !== holder.attachment) { delete drafts[hash]; return false; }
            error(holder.item.image ? 'Finish preparing the shared photo before sending.' : 'Send the shared text separately from a reply or attachment.'); return true;
        }
        var owner = RS.conversationOwner.snapshot(), text = input.value;
        if (!text.trim() && !holder.item.image) return true;
        holder.sending = true; clearTimeout(saveTimer);
        save(holder, text).then(function() { return edit(holder, 'sending'); }).then(function() {
            if (!RS.conversationOwner.isCurrent(owner) || !visible() || input.value !== text || sharedMediaBusy(holder)) {
                throw new Error('The conversation changed. The shared draft was kept.');
            }
            sendLxmfMessage(deliveryMethod, holder);
        }).catch(function(err) { holder.sending = false; if (RS.conversationOwner.isIdentityCurrent(owner)) error(err); });
        return true;
    }
    function accepted(holder) {
        if (!RS.conversationOwner.isIdentityCurrent(holder.owner)) return;
        var hash = holder.item.recipient;
        if (drafts[hash] === holder) delete drafts[hash];
        edit(holder, 'discard').catch(function() {
            error('Message queued. Its recovery copy remains in Shared items; do not send it again.');
        });
    }
    function failed(holder) {
        holder.sending = false;
        if (RS.conversationOwner.isIdentityCurrent(holder.owner)) {
            error('Message was not accepted. Your shared draft is kept in Shared items.');
        }
    }
    sheet._ratspeakDismiss = function() {
        if (selected && mode === 'contacts' && !selecting) setMode('recent');
        else close();
    };
    RS.gestures.attachDragDismiss(sheet, {
        axis: 'y', blockIfScrolled: false, parallaxOverlay: overlay,
        skipIf: function(e) { return !e.target.closest('.bottom-sheet-handle, .bottom-sheet-header') || !!e.target.closest('button'); },
        onCommit: close
    });
    overlay.addEventListener('click', close);
    document.getElementById('text-share-close').addEventListener('click', close);
    document.getElementById('text-share-discard').addEventListener('click', async function() {
        if (!selected || selecting) return;
        var holder = selected, session = epoch, owner = RS.conversationOwner.snapshot();
        var choice = await rsChoice({ title: 'Discard shared item?', message: 'This removes its saved recovery copy. Content already in a chat is not erased.',
            choices: [{ label: 'Keep item', value: 'keep' }, { label: 'Discard item', value: 'discard' }] });
        if (!current(owner, session) || choice !== 'discard') return;
        try {
            await edit(holder, 'discard');
            if (holder.item.recipient && drafts[holder.item.recipient] && drafts[holder.item.recipient].item.id === holder.item.id) delete drafts[holder.item.recipient];
            if (current(owner, session)) { selected = null; render(); }
        } catch (err) { if (current(owner, session)) error(err); }
    });
    pendingButton.addEventListener('click', function() { refresh(true); });
    search.addEventListener('input', function() { recipient = null; render(); });
    contacts.addEventListener('click', function() { setMode('contacts'); });
    back.addEventListener('click', function() { setMode('recent'); });
    confirm.addEventListener('click', function() {
        if (recipient && !selecting) return choose(recipient.hash);
    });
    input.addEventListener('input', function() {
        var holder = drafts[lxmfActiveContact];
        clearTimeout(saveTimer);
        if (!holder || holder.sending) return;
        var text = input.value;
        saveTimer = setTimeout(function() { save(holder, text).catch(error); }, 250);
    });
    document.addEventListener('visibilitychange', function() {
        if (!visible()) { close(); return; }
        needsRefresh = true; refresh(false);
    });
    window.addEventListener('pagehide', reset);
    RS.listen('identity_switching', reset).catch(function() {});
    RS.listen('identity_switched', reset).catch(function() {});
    RS.listen('identity_reset', reset).catch(function() {});
    RS.listen('text_shares_available', function() { needsRefresh = true; refresh(false); }).catch(function() {});
    RS.listen('stats_update', function() { if (needsRefresh && visible()) refresh(false); }).catch(function() {});
    RS.listen('system_status', function() { if (needsRefresh && visible()) refresh(false); }).catch(function() {});
    RS.textShares = { interceptSend: interceptSend, accepted: accepted, failed: failed, refresh: refresh, reset: reset,
        dispatched: function(holder) { if (drafts[holder.item.recipient] === holder) delete drafts[holder.item.recipient]; }
    };
    refresh(false);
})();
