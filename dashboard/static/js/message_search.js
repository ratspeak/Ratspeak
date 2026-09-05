// Message search owns its debounce, results and identity lifetime. Navigation
// clears it through the same input event as typing; identity replacement resets
// it explicitly so a late IPC response cannot revive another identity's text.
(function() {
    var input = document.getElementById('msg-search-input');
    var results = document.getElementById('msg-search-results');
    var conversations = document.getElementById('lxmf-conversations-list');
    if (!input || !results || !conversations) return;

    var timer = null;
    var revision = 0;

    function invalidate() {
        revision += 1;
        clearTimeout(timer);
        timer = null;
    }

    function reset() {
        invalidate();
        input.value = '';
        results.innerHTML = '';
        results.style.display = 'none';
        conversations.style.display = '';
    }

    function showStatus(message) {
        results.innerHTML = '<div class="lxmf-empty" role="status">' + escapeHtml(message) + '</div>';
    }

    input.addEventListener('input', function() {
        invalidate();
        var query = input.value.trim();
        if (query.length < 2) {
            results.innerHTML = '';
            results.style.display = 'none';
            conversations.style.display = '';
            return;
        }
        var request = revision;
        var owner = RS.conversationOwner.snapshot();
        function current() {
            return request === revision && input.value.trim() === query &&
                RS.conversationOwner.isIdentityCurrent(owner);
        }
        showStatus('Searching...');
        results.style.display = 'block';
        conversations.style.display = 'none';

        timer = setTimeout(function() {
            timer = null;
            if (!current()) return;
            RS.invoke('api_search_messages', { q: query }).then(function(messages) {
                if (!current()) return;
                if (!Array.isArray(messages) || messages.length === 0) {
                    showStatus('No results found.');
                    return;
                }
                // Reuse the conversation row typography and keyboard primitive.
                results.innerHTML = messages.map(function(message) {
                    var hash = message.direction === 'inbound' ? message.source : message.destination;
                    var name = _conversationNameInfo(hash, null, false);
                    return '<div class="conv-row" role="button" tabindex="0" data-hash="' + escapeHtml(hash) + '">' +
                        '<div class="conv-row-content"><div class="conv-row-top">' +
                            '<span class="conv-name' + (name.isHash ? ' is-hash' : '') + '">' +
                                ratspeakDisplayNameHtml(name.name, hash) + '</span>' +
                            '<span class="conv-time">' + formatConvTime(message.timestamp) + '</span>' +
                        '</div><div class="conv-row-bottom"><span class="conv-preview">' +
                            escapeHtml((message.content || '').substring(0, 80)) +
                        '</span></div></div></div>';
                }).join('');
                results.querySelectorAll('.conv-row').forEach(function(row) {
                    RS.ui.bindKeyboardActivation(row);
                    row.addEventListener('click', function() {
                        if (!current()) return;
                        var hash = row.getAttribute('data-hash');
                        reset();
                        if (hash) openConversationWith(hash);
                    });
                });
            }).catch(function(error) {
                if (!current()) return;
                RS.diag('error', 'Message search failed:', error);
                showStatus('Search failed.');
            });
        }, RS.config.DEBOUNCE_MESSAGE_SEARCH);
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
