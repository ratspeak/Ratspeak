// Several conversations with one contact (LXMF FIELD_THREAD). Main is the
// thread-less conversation every client shares; extra ones are local names
// over a 32-byte thread ID. A contact with only Main shows none of this.
(function() {
    var NAME_MAX = 40;
    var CHEVRON = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="m6 9 6 6 6-6"/></svg>';
    var PLUS = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 12h14"/><path d="M12 5v14"/></svg>';
    var MORE = '<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="5" cy="12" r="1"/><circle cx="12" cy="12" r="1"/><circle cx="19" cy="12" r="1"/></svg>';
    var CHECK = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M20 6 9 17l-5-5"/></svg>';
    var TAG = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12.6 2.6A2 2 0 0 0 11.2 2H4a2 2 0 0 0-2 2v7.2a2 2 0 0 0 .6 1.4l8.7 8.7a2.4 2.4 0 0 0 3.4 0l6.6-6.6a2.4 2.4 0 0 0 0-3.4z"/><circle cx="7.5" cy="7.5" r="1"/></svg>';
    var TRASH = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3 6h18"/><path d="M8 6V4h8v2"/><path d="M19 6l-1 14H6L5 6"/></svg>';

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

    function preview(t) {
        if (!t.last_message) return 'No messages yet';
        return (t.last_direction === 'outbound' ? 'You: ' : '') + t.last_message;
    }

    function isNew(t) {
        return t.started_by === 'contact' && !t.opened;
    }

    function render() {
        var header = document.getElementById('lxmf-chat-header');
        var chip = document.getElementById('lxmf-thread-chip');
        var tabs = document.getElementById('lxmf-thread-tabs');
        var note = document.getElementById('lxmf-thread-note');
        var show = hasExtra();
        if (header) header.classList.toggle('has-threads', show);
        if (chip) chip.hidden = !show;
        if (tabs) tabs.hidden = !show;
        var here = current();
        if (note) {
            var started = show && here && here.started_by === 'contact' && !here.name;
            note.hidden = !started;
            if (started) {
                note.innerHTML = '<span class="conv-thread-note-text"></span>' +
                    '<button type="button" class="conv-thread-note-rename">' + TAG + 'Rename</button>';
                note.querySelector('.conv-thread-note-text').textContent = contactName() + ' started this conversation';
                note.querySelector('button').addEventListener('click', function() { rename(here); });
            }
        }
        if (!show) return;
        var otherUnread = items().some(function(t) { return t.thread !== lxmfActiveThread && t.unread > 0; });
        if (chip) {
            chip.innerHTML = '<span class="conv-thread-chip-label"></span>' + CHEVRON +
                (otherUnread ? '<span class="conv-thread-chip-dot" aria-hidden="true"></span>' : '');
            chip.querySelector('.conv-thread-chip-label').textContent = here ? here.label : 'Main';
            chip.setAttribute('aria-label', 'Conversation: ' + (here ? here.label : 'Main') +
                (otherUnread ? '. Other conversations have unread messages.' : '') + ' Switch conversation');
        }
        if (tabs) renderTabs(tabs);
    }

    // Main, then creation order, so tabs never move while you work.
    function renderTabs(tabs) {
        tabs.innerHTML = '';
        items().forEach(function(t) {
            var tab = document.createElement('button');
            tab.type = 'button';
            tab.className = 'conv-thread-tab' + (t.thread === lxmfActiveThread ? ' is-selected' : '');
            tab.setAttribute('role', 'tab');
            tab.setAttribute('aria-selected', t.thread === lxmfActiveThread ? 'true' : 'false');
            tab.title = t.label;
            var name = document.createElement('span');
            name.className = 'conv-thread-tab-label';
            name.textContent = t.label;
            tab.appendChild(name);
            if (t.unread > 0 && t.thread !== lxmfActiveThread) {
                var badge = document.createElement('span');
                badge.className = 'conv-thread-badge';
                badge.textContent = t.unread;
                tab.appendChild(badge);
            }
            tab.addEventListener('click', function(e) {
                e.stopPropagation();
                switchConversationThread(t.thread);
            });
            if (t.thread) {
                tab.addEventListener('contextmenu', function(e) {
                    e.preventDefault();
                    e.stopPropagation();
                    openThreadActions(t, tab);
                });
            }
            tabs.appendChild(tab);
        });
        var add = document.createElement('button');
        add.type = 'button';
        add.className = 'conv-thread-tab-add';
        add.title = 'New conversation';
        add.setAttribute('aria-label', 'New conversation');
        add.innerHTML = PLUS;
        add.addEventListener('click', function(e) {
            e.stopPropagation();
            startNew();
        });
        tabs.appendChild(add);
        var selected = tabs.querySelector('.is-selected');
        if (selected && selected.scrollIntoView) selected.scrollIntoView({ block: 'nearest', inline: 'nearest' });
    }

    // Main first, then most recent activity.
    function switcherOrder() {
        var list = items().slice();
        var main = list.shift();
        list.sort(function(a, b) {
            return (b.timestamp || b.created_at || 0) - (a.timestamp || a.created_at || 0);
        });
        return main ? [main].concat(list) : list;
    }

    function openSwitcher() {
        if (!hasExtra()) return;
        var built = _rsBuildSheet({
            title: 'Conversations',
            ariaLabel: 'Conversations with ' + contactName(),
            sheetClass: 'conv-thread-sheet',
        });
        built.overlay.addEventListener('click', function(e) {
            if (e.target === built.overlay) built.dismiss(null);
        });
        var titleEl = built.sheet.querySelector('.bottom-sheet-title');
        if (titleEl) {
            var who = document.createElement('span');
            who.className = 'conv-thread-sheet-contact';
            who.textContent = ' · ' + contactName();
            titleEl.appendChild(who);
        }
        var list = document.createElement('div');
        list.className = 'conv-thread-list';
        list.setAttribute('role', 'listbox');
        switcherOrder().forEach(function(t) {
            var selected = t.thread === lxmfActiveThread;
            var row = document.createElement('div');
            row.className = 'conv-thread-row' + (selected ? ' is-selected' : '');
            row.setAttribute('role', 'option');
            row.setAttribute('aria-selected', selected ? 'true' : 'false');
            row.tabIndex = 0;
            row.innerHTML =
                '<span class="conv-thread-check">' + (selected ? CHECK : '') + '</span>' +
                '<span class="conv-thread-main">' +
                    '<span class="conv-thread-title"><span class="conv-thread-name"></span></span>' +
                    '<span class="conv-thread-preview"></span>' +
                '</span>' +
                '<span class="conv-thread-meta"><span class="conv-thread-time"></span></span>';
            row.querySelector('.conv-thread-name').textContent = t.label;
            row.querySelector('.conv-thread-preview').textContent = preview(t);
            row.querySelector('.conv-thread-time').textContent = t.timestamp ? formatConvTime(t.timestamp) : '';
            if (isNew(t)) {
                var tag = document.createElement('span');
                tag.className = 'conv-thread-new';
                tag.textContent = 'New';
                row.querySelector('.conv-thread-title').appendChild(tag);
            }
            if (t.unread > 0 && !selected) {
                var badge = document.createElement('span');
                badge.className = 'conv-thread-badge';
                badge.textContent = t.unread;
                row.querySelector('.conv-thread-meta').appendChild(badge);
            }
            var more = document.createElement('button');
            more.type = 'button';
            more.className = 'conv-thread-more';
            more.innerHTML = MORE;
            if (t.thread) {
                more.setAttribute('aria-label', 'Rename or delete ' + t.label);
                more.addEventListener('click', function(e) {
                    e.stopPropagation();
                    built.dismiss(null);
                    openThreadActions(t);
                });
            } else {
                more.hidden = true;
            }
            row.appendChild(more);
            row.addEventListener('click', function() {
                built.dismiss(null);
                switchConversationThread(t.thread);
            });
            if (RS.ui) RS.ui.bindKeyboardActivation(row);
            list.appendChild(row);
        });
        built.body.appendChild(list);
        var add = document.createElement('button');
        add.type = 'button';
        add.className = 'conv-thread-new-row';
        add.innerHTML = '<span class="conv-thread-new-icon">' + PLUS + '</span><span>New conversation</span>';
        add.addEventListener('click', function() {
            built.dismiss(null);
            startNew();
        });
        built.footer.appendChild(add);
        built.sheet.addEventListener('keydown', function(e) {
            if (e.key === 'Escape') { e.stopPropagation(); built.dismiss(null); }
        });
        built.present();
        var focus = list.querySelector('.is-selected') || list.firstChild;
        if (focus && (!(RS.ui && RS.ui.prefersKeyboardFocus) || RS.ui.prefersKeyboardFocus())) focus.focus();
    }

    function openThreadActions(t, trigger) {
        var actions = [
            { label: 'Rename conversation', icon: TAG, onSelect: function() { rename(t); } },
            { label: 'Delete conversation', icon: TRASH, danger: true, onSelect: function() { remove(t); } },
        ];
        if (trigger && RS.ui && typeof RS.ui.openActionMenu === 'function') {
            RS.ui.openActionMenu(trigger, actions, { title: t.label });
            return;
        }
        rsChoice({
            title: t.label,
            choices: [
                { label: 'Rename conversation', value: 'rename', icon: TAG },
                { label: 'Delete conversation', value: 'delete', icon: TRASH, danger: true },
            ],
        }).then(function(choice) {
            if (choice === 'rename') rename(t);
            if (choice === 'delete') remove(t);
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
            title: 'New conversation',
            message: 'With ' + contactName(),
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

    document.addEventListener('DOMContentLoaded', function() {
        var chip = document.getElementById('lxmf-thread-chip');
        if (chip) {
            chip.addEventListener('click', function(e) {
                e.stopPropagation();
                // Previews and counts move with every message; read them fresh.
                refresh().then(openSwitcher);
            });
        }
    });

    window.RS = window.RS || {};
    RS.threads = {
        render: render,
        refresh: refresh,
        hasExtra: hasExtra,
        current: current,
        openSwitcher: openSwitcher,
        startNew: startNew,
        rename: rename,
        remove: remove,
    };
})();
