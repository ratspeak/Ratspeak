// Message search owns its debounce, results and identity lifetime. Navigation
// clears it through the same input event as typing; identity replacement resets
// it explicitly so a late IPC response cannot revive another identity's text.
// Results group People (names and addresses) and Messages (text), the match in bold.
(function() {
    var input = document.getElementById('msg-search-input');
    var results = document.getElementById('msg-search-results');
    var conversations = document.getElementById('lxmf-conversations-list');
    var clear = document.getElementById('msg-search-clear');
    if (!input || !results || !conversations) return;

    var timer = null;
    var revision = 0;

    function invalidate() {
        revision += 1;
        clearTimeout(timer);
        timer = null;
    }

    function showList() {
        results.innerHTML = '';
        results.hidden = true;
        conversations.hidden = false;
    }

    function reset() {
        invalidate();
        input.value = '';
        if (clear) clear.hidden = true;
        showList();
    }

    function showStatus(html) {
        results.innerHTML = html;
    }

    // The text around the first match, the match in bold; long texts start near it.
    function highlight(text, query) {
        text = String(text || '').replace(/\s+/g, ' ').trim();
        var at = text.toLowerCase().indexOf(query.toLowerCase());
        if (at === -1) return escapeHtml(text);
        var start = at > 32 ? at - 20 : 0;
        return (start ? '…' : '') + escapeHtml(text.slice(start, at)) +
            '<b class="msg-hit">' + escapeHtml(text.slice(at, at + query.length)) + '</b>' +
            escapeHtml(text.slice(at + query.length));
    }

    function nameHtml(hash, info, query) {
        if (info.isHash) return '<span class="msg-title-hash">' + escapeHtml(info.name) + '</span>';
        return query ? highlight(info.name, query) : ratspeakDisplayNameHtml(info.name, hash);
    }

    // People: contacts and conversations whose name or address matches.
    function people(query) {
        var q = query.toLowerCase();
        var seen = {};
        var found = [];
        function consider(hash, displayName, isContact) {
            hash = _canonicalConversationHash(hash);
            if (!hash || seen[hash]) return;
            var info = _conversationNameInfo(hash, displayName, isContact);
            var match = (!info.isHash && info.name.toLowerCase().indexOf(q) !== -1) || hash.indexOf(q.replace(/\s+/g, '')) !== -1;
            if (!match) return;
            seen[hash] = true;
            found.push({ hash: hash, info: info });
        }
        (lxmfContacts || []).forEach(function(c) { consider(c.hash, c.display_name, true); });
        (lxmfConversations || []).forEach(function(c) { consider(c.hash, c.display_name, c.is_contact); });
        return found.slice(0, 8);
    }

    function personRow(p, query) {
        var presence = _conversationPresence(p.hash);
        return '<li><button type="button" class="ui-row" data-hash="' + escapeHtml(p.hash) + '">' +
            RS.kit.avatar(p.hash, 'row', presence.active ? 'active' : null, presence.heard) +
            '<span class="ui-row-main"><span class="ui-row-top"><span class="ui-row-title">' + nameHtml(p.hash, p.info, query) + '</span></span>' +
            '<span class="ui-row-sub"><span class="t ui-hash">' + escapeHtml(RS.kit.hash.short(p.hash)) + '</span></span></span></button></li>';
    }

    function messageRow(message, query) {
        var hash = message.direction === 'inbound' ? message.source : message.destination;
        var info = _conversationNameInfo(hash, null, false);
        var label = message.thread_label || '';
        return '<li><button type="button" class="ui-row" data-hash="' + escapeHtml(hash) + '" data-thread="' + escapeHtml(message.thread || '') + '" data-id="' + escapeHtml(message.id || '') + '">' +
            RS.kit.avatar(hash, 'row') +
            '<span class="ui-row-main"><span class="ui-row-top"><span class="ui-row-title">' + nameHtml(hash, info, '') + '</span>' +
            '<span class="ui-time" title="' + escapeHtml(RS.kit.time.full(message.timestamp)) + '">' + escapeHtml(RS.kit.time.row(message.timestamp)) + '</span></span>' +
            '<span class="ui-row-sub"><span class="t">' + (label ? '<span class="msg-thread">' + escapeHtml(label) + '</span> · ' : '') +
            highlight(_messagePlainContent(message, !!message.audio), query) + '</span></span></span></button></li>';
    }

    function render(query, messages) {
        var who = people(query);
        var html = '';
        if (who.length) {
            html += '<ul class="ui-list" aria-label="People"><li class="ui-caption">People</li>' +
                who.map(function(p) { return personRow(p, query); }).join('') + '</ul>';
        }
        if (messages.length) {
            html += '<ul class="ui-list" aria-label="Messages"><li class="ui-caption">Messages</li>' +
                messages.map(function(m) { return messageRow(m, query); }).join('') + '</ul>';
        }
        if (!html) {
            html = '<div class="ui-empty"><h3 class="ui-empty-title">No messages match “' + escapeHtml(query) + '”</h3>' +
                '<p class="ui-empty-body">Search checks names and message text.</p>' +
                '<div class="ui-empty-actions"><button type="button" class="ui-link" data-search-action="clear">Clear search</button></div></div>';
        }
        showStatus(html);
    }

    input.addEventListener('input', function() {
        invalidate();
        var query = input.value.trim();
        if (clear) clear.hidden = !input.value;
        if (query.length < 2) {
            showList();
            return;
        }
        var request = revision;
        var owner = RS.conversationOwner.snapshot();
        function current() {
            return request === revision && input.value.trim() === query &&
                RS.conversationOwner.isIdentityCurrent(owner);
        }
        showStatus('<div class="ui-empty ui-empty--inline" role="status"><span class="ui-spin ui-spin--ring" aria-hidden="true"></span><span>Searching</span></div>');
        results.hidden = false;
        conversations.hidden = true;

        timer = setTimeout(function() {
            timer = null;
            if (!current()) return;
            RS.invoke('api_search_messages', { q: query }).then(function(messages) {
                if (!current()) return;
                render(query, Array.isArray(messages) ? messages : []);
            }).catch(function(error) {
                if (!current()) return;
                RS.diag('error', 'Message search failed:', error);
                showStatus('<div class="ui-empty"><h3 class="ui-empty-title">Couldn’t search your messages</h3>' +
                    '<p class="ui-empty-body">Try again in a moment.</p></div>');
            });
        }, RS.config.DEBOUNCE_MESSAGE_SEARCH);
    });

    results.addEventListener('click', function(e) {
        if (e.target.closest('[data-search-action="clear"]')) {
            reset();
            input.focus();
            return;
        }
        var row = e.target.closest('.ui-row[data-hash]');
        if (!row) return;
        var hash = row.getAttribute('data-hash');
        var id = row.getAttribute('data-id');
        var thread = row.getAttribute('data-thread');
        reset();
        if (!hash) return;
        if (id && typeof openConversationAtMessage === 'function') openConversationAtMessage(hash, id, null, thread || '');
        else openConversationWith(hash);
    });

    if (clear) clear.addEventListener('click', function() {
        reset();
        input.focus();
    });

    input.addEventListener('keydown', function(event) {
        if (event.isComposing) return;
        if (event.key === 'Escape') {
            event.preventDefault();
            reset();
        } else if (event.key === 'Enter') {
            event.preventDefault();
            input.blur();
        }
    });
    RS.messageSearch = { reset: reset };
})();
