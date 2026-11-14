// Several conversations with one contact (LXMF FIELD_THREAD). Main is the
// thread-less conversation every client shares; extra ones are local names
// over a 32-byte thread ID. A contact with only Main shows none of this.
// They are one chip row under the chat header on every size.
(function() {
    var NAME_MAX = 40;
    var refreshSeq = 0;

    function items() {
        return (window.lxmfActiveThreads && Array.isArray(lxmfActiveThreads.items)) ? lxmfActiveThreads.items : [];
    }

    function hasExtra() {
        return !!lxmfActiveContact && items().length > 1;
    }

    function current() {
        return items().find(function(t) { return t.thread === lxmfActiveThread; }) || null;
    }

    function contactName() {
        return _conversationNameInfo(lxmfActiveContact, null, false).name;
    }

    function firstName() {
        var info = _conversationNameInfo(lxmfActiveContact, null, false);
        return info.isHash ? info.name : String(info.name || '').trim().split(/\s+/)[0];
    }

    function render() {
        var row = document.getElementById('lxmf-thread-tabs');
        var note = document.getElementById('lxmf-thread-note');
        var show = hasExtra();
        if (row) row.hidden = !show;
        var here = current();
        if (note) {
            var started = show && here && here.started_by === 'contact' && !here.name;
            note.hidden = !started;
            if (started) {
                note.innerHTML = '<span></span> <button type="button" class="ui-link">Rename it</button>';
                note.firstChild.textContent = firstName() + ' started this conversation.';
                note.querySelector('button').addEventListener('click', function() { rename(here); });
            }
        }
        if (show && row) renderChips(row);
    }

    // Main, then creation order, so chips never move while you work.
    function renderChips(row) {
        var focusedThread = row.contains(document.activeElement) ? document.activeElement.getAttribute('data-thread') : null;
        var who = contactName();
        row.innerHTML = '<div class="ui-chips ui-chips--scroll" role="radiogroup"></div>' +
            '<button type="button" class="ui-icon-btn ui-icon-btn--sm msg-thread-new" title="New conversation">' + RS.kit.icon('plus') + '</button>';
        var chips = row.firstChild;
        chips.setAttribute('aria-label', 'Conversations with ' + who);
        row.lastChild.setAttribute('aria-label', 'New conversation with ' + who);
        items().forEach(function(t) {
            var on = t.thread === lxmfActiveThread;
            var chip = document.createElement('button');
            chip.type = 'button';
            chip.className = 'ui-chip';
            chip.setAttribute('role', 'radio');
            chip.setAttribute('aria-checked', on ? 'true' : 'false');
            chip.setAttribute('data-thread', t.thread || '');
            chip.tabIndex = on ? 0 : -1;
            chip.innerHTML = (on ? RS.kit.icon('check') : '') + '<span></span>';
            chip.querySelector('span').textContent = t.label;
            if (t.unread > 0 && !on) {
                var count = document.createElement('span');
                count.className = 'ui-count';
                count.textContent = t.unread > 99 ? '99+' : String(t.unread);
                count.setAttribute('aria-label', t.unread + ' unread');
                chip.appendChild(count);
            }
            if (t.thread) {
                chip.title = on ? 'Rename or delete' : t.label;
                chip.addEventListener('contextmenu', function(e) {
                    e.preventDefault();
                    e.stopPropagation();
                    openThreadActions(t, chip);
                });
                RS.gestures.attachLongPress(chip, {
                    duration: 500,
                    moveCancelPx: 12,
                    onFire: function() { chip._rsHeld = true; openThreadActions(t, chip); }
                });
            }
            chip.addEventListener('click', function(e) {
                e.stopPropagation();
                if (chip._rsHeld) { chip._rsHeld = false; return; }
                // The chosen chip opens its own Rename and Delete.
                if (on) {
                    if (t.thread) openThreadActions(t, chip);
                    return;
                }
                switchConversationThread(t.thread);
            });
            chips.appendChild(chip);
        });
        chips.addEventListener('keydown', function(e) {
            var all = Array.prototype.slice.call(chips.querySelectorAll('.ui-chip'));
            var at = all.indexOf(document.activeElement);
            var to = { ArrowRight: at + 1, ArrowDown: at + 1, ArrowLeft: at - 1, ArrowUp: at - 1, Home: 0, End: all.length - 1 }[e.key];
            if (at === -1 || to == null) return;
            e.preventDefault();
            to = (to + all.length) % all.length;
            all[to].focus();
            all[to].click();
        });
        row.lastChild.addEventListener('click', function(e) {
            e.stopPropagation();
            startNew();
        });
        var selected = chips.querySelector('[aria-checked="true"]');
        if (selected && selected.scrollIntoView) selected.scrollIntoView({ block: 'nearest', inline: 'nearest' });
        if (focusedThread !== null) {
            var again = chips.querySelector('[data-thread="' + focusedThread + '"]');
            if (again) again.focus({ preventScroll: true });
        }
    }

    // Rename and delete: a menu from the chip on desktop, a sheet on a phone.
    function openThreadActions(t, chip) {
        RS.kit.menu(chip, {
            title: t.label,
            align: 'start',
            items: [
                { label: 'Rename conversation', icon: 'tag', onSelect: function() { rename(t); } },
                { label: 'Delete conversation', icon: 'trash', danger: true, onSelect: function() { remove(t); } },
            ],
            onClose: function() { chip.removeAttribute('aria-expanded'); }
        });
    }

    function owner() {
        return RS.conversationOwner.snapshot();
    }

    function sameContact(snapshot) {
        return RS.conversationOwner.isIdentityCurrent(snapshot) &&
            _canonicalConversationHash(lxmfActiveContact) === snapshot.hash;
    }

    function applyThreads(snapshot, threads) {
        if (!threads || !sameContact(snapshot)) return;
        lxmfActiveThreads = threads;
        render();
    }

    function startNew() {
        if (!lxmfActiveContact) return;
        var snapshot = owner();
        var next = (lxmfActiveThreads && lxmfActiveThreads.next_number) || 2;
        var suggested = 'Conversation ' + next;
        rsPrompt({
            title: 'New conversation with ' + firstName(),
            label: 'Name',
            help: 'Only you see this name.',
            defaultValue: suggested,
            confirmText: 'Start',
            maxLength: NAME_MAX,
            inputClass: 'rs-dialog-input--text',
        }).then(function(value) {
            if (value === null || !sameContact(snapshot)) return;
            var name = value.trim();
            // An untouched default keeps the number the backend assigns.
            return RS.invoke('create_conversation_thread', {
                hash: snapshot.hash,
                name: name && name !== suggested ? name : null,
            }).then(function(resp) {
                if (!resp || !sameContact(snapshot)) return;
                lxmfActiveThreads = resp.threads || lxmfActiveThreads;
                switchConversationThread(resp.thread);
                var input = document.getElementById('lxmf-input');
                if (input && (!(RS.ui && RS.ui.prefersKeyboardFocus) || RS.ui.prefersKeyboardFocus())) input.focus();
            });
        }).catch(function(err) {
            showToast((err && err.message) || 'Couldn\'t start the conversation', 'toast-error', 3500);
        });
    }

    function rename(t) {
        if (!t || !t.thread || !lxmfActiveContact) return;
        var snapshot = owner();
        rsPrompt({
            title: 'Rename conversation',
            message: 'With ' + contactName(),
            defaultValue: t.label,
            confirmText: 'Save',
            maxLength: NAME_MAX,
            inputClass: 'rs-dialog-input--text',
        }).then(function(value) {
            if (value === null || !sameContact(snapshot)) return;
            return RS.invoke('rename_conversation_thread', {
                hash: snapshot.hash,
                thread: t.thread,
                name: value.trim() || null,
            }).then(function(resp) { applyThreads(snapshot, resp && resp.threads); });
        }).catch(function(err) {
            showToast((err && err.message) || 'Couldn\'t rename the conversation', 'toast-error', 3500);
        });
    }

    function remove(t) {
        if (!t || !lxmfActiveContact) return;
        var snapshot = owner();
        var main = !t.thread;
        rsConfirm({
            title: main ? 'Clear Main' : 'Delete conversation',
            message: main
                ? 'Delete the messages in Main on this device? Your other conversations stay.'
                : 'Delete "' + t.label + '" and its messages on this device?',
            confirmText: main ? 'Clear' : 'Delete',
            danger: true,
        }).then(function(ok) {
            if (!ok || !sameContact(snapshot)) return;
            return RS.invoke('delete_conversation', { hash: snapshot.hash, thread: t.thread });
        }).catch(function(err) {
            showToast((err && err.message) || 'Couldn\'t delete the conversation', 'toast-error', 3500);
        });
    }

    function refresh() {
        if (!lxmfActiveContact) return Promise.resolve();
        var snapshot = owner();
        var seq = ++refreshSeq;
        return RS.invoke('get_conversation_threads', { hash: snapshot.hash }).then(function(threads) {
            if (seq === refreshSeq) applyThreads(snapshot, threads);
        }).catch(function() {});
    }

    window.RS = window.RS || {};
    RS.threads = {
        render: render,
        refresh: refresh,
        hasExtra: hasExtra,
        current: current,
        startNew: startNew,
        rename: rename,
        remove: remove,
    };
})();
