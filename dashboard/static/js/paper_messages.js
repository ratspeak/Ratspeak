// LXMF paper messages: the shared decode result, the "Open paper message"
// sheet, and the error vocabulary. Preview never files; Add to chat does.
(function() {
    'use strict';

    var PAPER_URI_RE = /^lxm:\/\//i;
    var PAPER_MAX_CHARS = 4096;
    var ERROR_TEXT = {
        paper_invalid: 'That isn\'t a valid paper message.',
        paper_not_for_you: 'This paper message isn\'t addressed to you.',
        paper_decrypt_failed: 'Couldn\'t decrypt this paper message. It may be incomplete or damaged.',
    };
    var SCAN_ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="M4 7V4h3M17 4h3v3M20 17v3h-3M7 20H4v-3"/><rect x="8" y="8" width="3" height="3"/><rect x="14" y="8" width="2" height="2"/><rect x="8" y="14" width="2" height="2"/><path d="M14 14h2v2h2v2h-4z"/></svg>';
    var PASTE_ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="M16 4h2a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2"/><rect x="8" y="2" width="8" height="4" rx="1"/></svg>';
    var CHECK_ICON = '<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><polyline points="3 8 7 12 13 4"/></svg>';

    function parsedErrorPayload(err) {
        var message = err && typeof err.message === 'string' ? err.message : (typeof err === 'string' ? err : '');
        var start = message.indexOf('{');
        if (start === -1) return null;
        try {
            var parsed = JSON.parse(message.slice(start));
            return parsed && typeof parsed === 'object' ? parsed : null;
        } catch (_) {
            return null;
        }
    }

    // Reads the paper error code from a `code` field, a JSON body, or a "paper_x" token.
    function paperErrorCode(err) {
        if (!err) return '';
        if (typeof err.code === 'string' && err.code && err.code !== 'error' &&
                (err.code.indexOf('paper_') === 0 || err.code === 'locked')) return err.code;
        var parsed = parsedErrorPayload(err);
        if (parsed && typeof parsed.code === 'string') return parsed.code;
        var message = typeof err === 'string' ? err : String(err.message || '');
        var token = /\b(paper_[a-z_]+)\b/.exec(message);
        if (token) return token[1];
        return typeof err.code === 'string' ? err.code : '';
    }

    function paperErrorDetail(err, key) {
        if (!err) return null;
        if (err[key] !== undefined && err[key] !== null) return err[key];
        if (err.details && err.details[key] !== undefined) return err.details[key];
        var parsed = parsedErrorPayload(err);
        if (parsed) {
            if (parsed[key] !== undefined) return parsed[key];
            if (parsed.details && parsed.details[key] !== undefined) return parsed.details[key];
        }
        var message = String(err.message || err || '');
        var named = new RegExp(key + '["\'\\s:=]+(\\d+)').exec(message);
        if (named) return Number(named[1]);
        var counted = /(\d[\d,]*)\s*(?:characters|chars)\b/i.exec(message);
        return counted ? Number(counted[1].replace(/,/g, '')) : null;
    }

    function paperErrorText(err) {
        var code = paperErrorCode(err);
        if (ERROR_TEXT[code]) return ERROR_TEXT[code];
        if (err && err.message && !/\bpaper_[a-z_]+\b/.test(err.message)) return err.message;
        return ERROR_TEXT.paper_invalid;
    }

    function previewPaper(uri) {
        return RS.invoke('preview_paper_message', { uri: uri });
    }

    function shortAddress(hash) {
        return typeof shortHash === 'function' ? shortHash(hash, 8, 4) : String(hash || '');
    }

    function senderName(preview) {
        var hash = preview.source_hash || '';
        var contact = typeof _lookupContactRecord === 'function' ? _lookupContactRecord(hash) : null;
        if (contact && contact.display_name) return contact.display_name;
        if (preview.display_name) return preview.display_name;
        if (typeof _conversationNameInfo === 'function') return _conversationNameInfo(hash, null, false).name;
        return shortAddress(hash);
    }

    function sentLabel(ts) {
        if (!ts) return '';
        var date = typeof _formatDateLabel === 'function' ? _formatDateLabel(ts) : '';
        var time = typeof formatTime === 'function' ? formatTime(ts) : '';
        return date && time ? date + ', ' + time : (date || time);
    }

    function previewBody(preview) {
        var strip = window.RS && RS.markdown ? RS.markdown.strip : function(text) { return String(text || ''); };
        var title = String(preview.title || '').trim();
        var body = strip(String(preview.content || ''));
        return title ? title + (body.trim() ? '\n\n' + body : '') : body;
    }

    function signatureHtml(signature) {
        if (signature === 'verified') {
            return '<div class="paper-result-status paper-result-status-verified">' + CHECK_ICON + '<span>Verified sender</span></div>';
        }
        if (signature === 'invalid') {
            return '<div class="paper-result-status paper-result-status-invalid">Signature doesn\'t match</div>' +
                '<div class="paper-result-helper">This message may have been altered.</div>';
        }
        return '<div class="paper-result-status paper-result-status-unknown">Unverified sender</div>' +
            '<div class="paper-result-helper">Their identity isn\'t known yet, so the signature can\'t be checked.</div>';
    }

    function revealAdded(preview, added) {
        var hash = (added && added.source_hash) || preview.source_hash;
        var id = (added && added.message_id) || preview.message_id;
        var ts = (added && added.timestamp) || preview.timestamp;
        setTimeout(function() {
            if (typeof openConversationAtMessage === 'function') openConversationAtMessage(hash, id, ts);
        }, 220);
    }

    // The one decode result view: scanner body, decoder sheet, and chat links.
    // opts: { close: function }.
    function renderPaperResult(parent, uri, preview, opts) {
        opts = opts || {};
        preview = preview || {};
        var close = typeof opts.close === 'function' ? opts.close : function() {};
        var hash = preview.source_hash || '';
        var duplicate = preview.status === 'duplicate';
        var blocked = !!preview.blocked;
        var invalidSignature = preview.signature === 'invalid';
        var canAdd = !blocked && !invalidSignature;
        var note = blocked ? 'You blocked this sender.' : (duplicate ? 'Already in this chat' : '');
        parent.innerHTML =
            '<div class="contact-scan-success paper-result">' +
                '<div class="contact-scan-avatar">' + (typeof identityAvatar === 'function' && hash ? identityAvatar(hash, 72) : '') + '</div>' +
                '<div class="contact-scan-name">' + escapeHtml(senderName(preview)) + '</div>' +
                signatureHtml(preview.signature) +
                '<div class="contact-card-detail-list">' +
                    '<div class="contact-card-detail-row"><span>Sent</span><code class="paper-result-plain">' + escapeHtml(sentLabel(preview.timestamp)) + '</code></div>' +
                    '<div class="contact-card-detail-row"><span>From</span><code>' + escapeHtml(shortAddress(hash)) + '</code></div>' +
                '</div>' +
                '<div class="contact-share-address-block paper-result-preview" tabindex="0" aria-label="Message preview"></div>' +
                (note ? '<div class="paper-result-note" role="status">' + escapeHtml(note) + '</div>' : '') +
                '<div class="contact-scan-actions">' +
                    '<button class="nr-btn nr-btn-ghost paper-result-cancel" type="button">Cancel</button>' +
                    '<button class="nr-btn nr-btn-primary paper-result-add" type="button"' + (canAdd ? '' : ' disabled') + '>' +
                        (duplicate && canAdd ? 'Open chat' : 'Add to chat') + '</button>' +
                '</div>' +
            '</div>';
        parent.querySelector('.paper-result-preview').textContent = previewBody(preview);
        parent.querySelector('.paper-result-cancel').addEventListener('click', function() { close('cancel'); });
        var add = parent.querySelector('.paper-result-add');
        add.addEventListener('click', function() {
            if (!canAdd || add.disabled) return;
            if (duplicate) {
                close('open');
                revealAdded(preview, null);
                return;
            }
            add.disabled = true;
            add.textContent = 'Adding…';
            RS.invoke('add_paper_message', { uri: uri }).then(function(result) {
                if (result && result.status === 'duplicate') showToast('Already in this chat', 'toast-info', 2500);
                close('added');
                revealAdded(preview, result);
            }).catch(function(err) {
                add.disabled = false;
                add.textContent = 'Add to chat';
                if (paperErrorCode(err) === 'locked') return;
                showToast(paperErrorText(err), 'toast-error', 3500);
            });
        });
        return add;
    }

    function readClipboardText() {
        if (!navigator.clipboard || typeof navigator.clipboard.readText !== 'function') {
            return Promise.reject(new Error('Clipboard unavailable'));
        }
        return navigator.clipboard.readText();
    }

    // opts: { uri, autoReview, onClose(action) }. Not gated by the setting.
    function openPaperDecoder(opts) {
        opts = opts && typeof opts === 'object' && !opts.type ? opts : {};
        if (typeof _rsBuildSheet !== 'function') return null;
        var action = 'dismiss';
        var clipboardCandidate = null;
        var built = _rsBuildSheet({ title: 'Open paper message' }, function() {
            clipboardCandidate = null;
            if (typeof opts.onClose === 'function') opts.onClose(action);
        });
        built.sheet.classList.add('channel-open-link-sheet', 'paper-decoder-sheet');

        var form = document.createElement('div');
        form.className = 'paper-decoder-form';
        var copy = document.createElement('p');
        copy.className = 'channel-sheet-copy';
        copy.textContent = 'Paste a paper link or scan its QR.';
        form.appendChild(copy);

        var ios = typeof isIOS === 'function' && isIOS();
        var tools = document.createElement('div');
        tools.className = 'channel-link-methods';
        var paste = ios
            ? _channelsBuildLinkMethod('Paste', 'Paste a copied paper link', PASTE_ICON)
            : _channelsBuildLinkMethod('Paste message from clipboard', 'A paper link is on your clipboard', PASTE_ICON);
        paste.classList.add('paper-decoder-paste');
        paste.hidden = !ios || !navigator.clipboard || typeof navigator.clipboard.readText !== 'function';
        tools.appendChild(paste);
        var scan = _channelsBuildLinkMethod('Scan QR', 'Use the camera or choose an image', SCAN_ICON);
        scan.hidden = !RS.qr || typeof RS.qr.openScanner !== 'function';
        scan.addEventListener('click', function() {
            action = 'scan';
            built.dismiss();
            setTimeout(function() { RS.qr.openScanner(); }, 220);
        });
        tools.appendChild(scan);
        form.appendChild(tools);

        var input = document.createElement('textarea');
        input.className = 'nr-input-sm channel-share-input mono';
        input.rows = 4;
        input.maxLength = PAPER_MAX_CHARS;
        input.placeholder = 'lxm://…';
        input.autocomplete = 'off';
        input.setAttribute('autocorrect', 'off');
        input.setAttribute('autocapitalize', 'none');
        input.setAttribute('spellcheck', 'false');
        input.setAttribute('writingsuggestions', 'false');
        input.value = opts.uri || '';
        form.appendChild(_channelsSheetField('Paper link', input));

        var error = document.createElement('div');
        error.className = 'channel-sheet-error';
        error.setAttribute('aria-live', 'polite');
        form.appendChild(error);
        built.body.appendChild(form);

        var result = document.createElement('div');
        result.className = 'paper-decoder-result';
        result.hidden = true;
        built.body.appendChild(result);

        var checking = document.createElement('div');
        checking.className = 'paper-decoder-checking';
        checking.setAttribute('role', 'status');
        checking.textContent = 'Checking…';
        checking.hidden = true;
        built.body.appendChild(checking);

        var cancel = document.createElement('button');
        cancel.type = 'button';
        cancel.className = 'nr-btn nr-btn-secondary';
        cancel.textContent = 'Cancel';
        cancel.addEventListener('click', function() { built.dismiss(); });
        var review = document.createElement('button');
        review.type = 'button';
        review.className = 'nr-btn nr-btn-primary';
        review.textContent = 'Review';
        review.addEventListener('click', runReview);
        built.footer.appendChild(cancel);
        built.footer.appendChild(review);

        function showForm() {
            form.hidden = false;
            checking.hidden = true;
            result.hidden = true;
            built.footer.hidden = false;
        }

        function runReview() {
            var raw = input.value.trim();
            if (!raw) {
                error.textContent = 'Paste a paper link or scan its QR.';
                input.focus();
                return;
            }
            review.disabled = true;
            review.textContent = 'Checking…';
            error.textContent = '';
            previewPaper(raw).then(function(preview) {
                form.hidden = true;
                checking.hidden = true;
                built.footer.hidden = true;
                result.hidden = false;
                var primary = renderPaperResult(result, raw, preview, {
                    close: function(how) {
                        action = how || 'dismiss';
                        built.dismiss();
                    }
                });
                if (RS.ui && typeof RS.ui.focusAfterUpdate === 'function') {
                    RS.ui.focusAfterUpdate(primary.disabled ? result.querySelector('.paper-result-cancel') : primary);
                }
            }).catch(function(err) {
                showForm();
                if (paperErrorCode(err) !== 'locked') error.textContent = paperErrorText(err);
            }).then(function() {
                review.disabled = false;
                review.textContent = 'Review';
            });
        }

        paste.addEventListener('click', function() {
            if (!ios && clipboardCandidate) {
                input.value = clipboardCandidate;
                clipboardCandidate = null;
                runReview();
                return;
            }
            readClipboardText().then(function(text) {
                input.value = String(text || '').trim().slice(0, PAPER_MAX_CHARS);
                runReview();
            }).catch(function() {
                error.textContent = 'Clipboard access was unavailable. Paste the link into the field.';
                input.focus();
            });
        });

        input.addEventListener('paste', function() {
            setTimeout(function() {
                if (PAPER_URI_RE.test(input.value.trim()) && !review.disabled) runReview();
            }, 0);
        });

        function focusInitial() {
            setTimeout(function() {
                var target = !paste.hidden ? paste : input;
                if (RS.ui && typeof RS.ui.focusAfterUpdate === 'function') RS.ui.focusAfterUpdate(target);
                else target.focus();
            }, 250);
        }

        _channelsPresentSheet(built, null);
        if (opts.uri) {
            if (opts.autoReview !== false) {
                form.hidden = true;
                built.footer.hidden = true;
                checking.hidden = false;
                runReview();
            } else {
                focusInitial();
            }
        } else if (!ios) {
            // Desktop and Android read once, on open; the text is never kept past the sheet.
            readClipboardText().then(function(text) {
                var candidate = String(text || '').trim();
                if (PAPER_URI_RE.test(candidate) && candidate.length <= PAPER_MAX_CHARS) {
                    clipboardCandidate = candidate;
                    paste.hidden = false;
                }
            }).catch(function() {}).then(focusInitial);
        } else {
            focusInitial();
        }
        return built;
    }

    window.paperErrorCode = paperErrorCode;
    window.paperErrorDetail = paperErrorDetail;
    window.openPaperDecoder = openPaperDecoder;
    window.RS.paper = {
        preview: previewPaper,
        renderResult: renderPaperResult,
        errorCode: paperErrorCode,
        errorDetail: paperErrorDetail,
        errorText: paperErrorText,
        openDecoder: openPaperDecoder,
        isUri: function(value) { return PAPER_URI_RE.test(String(value || '').trim()); },
    };
})();
