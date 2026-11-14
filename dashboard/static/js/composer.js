// The message composer shared by Messages and Channels: one pill with attach,
// the text, and one trailing slot that holds Record (empty) or Send (text or an
// attachment). Reply and attachment previews sit inside the pill above the text.
(function() {
    'use strict';
    window.RS = window.RS || {};
    var RS = window.RS;
    var composer = RS.composer = RS.composer || {};

    function kit() { return RS.kit; }
    function esc(value) { return kit().esc(value); }
    function icon(name) { return kit().icon(name); }
    function attr(name, value) { return value ? ' ' + name + '="' + esc(value) + '"' : ''; }

    // o: { id, inputId, sendId, recordId, attachId, placeholder, label, attach (true), voice (true),
    //      disabled, reason, busy }. Messages writes the same markup in index.html.
    composer.html = function(o) {
        o = o || {};
        var attach = o.attach !== false;
        var voice = o.voice !== false;
        var off = !!o.disabled;
        var reasonId = (o.id || 'cmp') + '-reason';
        return '<div class="cmp' + (off ? ' is-disabled' : '') + '"' + attr('id', o.id) + '>' +
            '<p class="cmp-reason" id="' + esc(reasonId) + '" role="status"' + (off && o.reason ? '' : ' hidden') + '>' +
                '<span class="ui-spin" aria-hidden="true"' + (o.busy ? '' : ' hidden') + '></span><span class="cmp-reason-text">' + esc(o.reason || '') + '</span></p>' +
            '<div class="cmp-pill">' +
                '<div class="cmp-extras" hidden></div>' +
                '<div class="cmp-row">' +
                    (attach ? '<span class="cmp-slot"><button type="button" class="ui-icon-btn cmp-attach"' + attr('id', o.attachId) + ' aria-label="Attach" title="Attach" aria-haspopup="menu" aria-expanded="false"' + (off ? ' disabled' : '') + '>' + icon('plus') + '</button></span>' : '') +
                    '<textarea class="cmp-input" rows="1"' + attr('id', o.inputId) + ' aria-label="' + esc(o.label || 'Message') + '" placeholder="' + esc(o.placeholder || 'Message') + '"' +
                        (off ? ' disabled' : '') + (off && o.reason ? ' aria-describedby="' + esc(reasonId) + '"' : '') + '></textarea>' +
                    '<span class="cmp-slot">' +
                        (voice ? '<button type="button" class="ui-icon-btn cmp-record"' + attr('id', o.recordId) + ' aria-label="Record a voice message" title="Record a voice message"' + (off ? ' disabled' : '') + '>' + icon('mic') + '</button>' : '') +
                        '<button type="button" class="ui-send cmp-send"' + attr('id', o.sendId) + ' aria-label="Send" title="Send"' + (voice ? ' hidden' : '') + ' disabled>' + icon('arrow-up') + '</button>' +
                    '</span>' +
                '</div>' +
            '</div>' +
        '</div>';
    };

    // A preview inside the pill. o: { title (reply: "Replying to Ada"), text, meta, thumb (url), tile (glyph),
    // closeLabel }. Its × carries data-cmp-close.
    composer.preview = function(o) {
        o = o || {};
        var lead = o.thumb ? '<img class="cmp-thumb" src="' + esc(o.thumb) + '" alt="">' :
            o.tile ? '<span class="ui-tile">' + icon(o.tile) + '</span>' : '';
        return lead + '<span class="cmp-preview-text">' + (o.title ? '<b>' + esc(o.title) + '</b>' : '') +
            '<span>' + esc(o.text || '') + '</span>' + (o.meta ? '<small>' + esc(o.meta) + '</small>' : '') + '</span>' +
            '<button type="button" class="ui-icon-btn ui-icon-btn--sm" data-cmp-close aria-label="' + esc(o.closeLabel || 'Remove') + '" title="' + esc(o.closeLabel || 'Remove') + '">' + icon('x') + '</button>';
    };

    function rootOf(el) {
        if (!el) return null;
        if (typeof el === 'string') el = document.getElementById(el);
        return el && el.closest ? el.closest('.cmp') || el : null;
    }

    // The extras well shows only while a preview inside it does.
    composer.syncExtras = function(root) {
        root = rootOf(root);
        var extras = root && root.querySelector('.cmp-extras');
        if (!extras) return;
        extras.hidden = !Array.prototype.some.call(extras.children, function(child) { return !child.hidden; });
    };

    // Record shows while the field is empty; Send once there is text or an attachment.
    composer.sync = function(root, opts) {
        root = rootOf(root);
        if (!root) return;
        opts = opts || {};
        var input = root.querySelector('.cmp-input');
        var record = root.querySelector('.cmp-record');
        var send = root.querySelector('.cmp-send');
        var has = !!(input && input.value.trim()) || !!opts.attachment;
        var canRecord = !!record && opts.voice !== false && !has;
        if (record) record.hidden = !canRecord;
        if (send) {
            send.hidden = canRecord;
            send.disabled = root.classList.contains('is-disabled') || (!has && !opts.allowEmpty);
        }
        composer.syncExtras(root);
    };

    // Disabled: the pill dims and one line above it says why. o: { reason, busy }.
    composer.setDisabled = function(root, disabled, o) {
        root = rootOf(root);
        if (!root) return;
        o = o || {};
        root.classList.toggle('is-disabled', !!disabled);
        var reason = root.querySelector('.cmp-reason');
        var input = root.querySelector('.cmp-input');
        if (reason) {
            reason.hidden = !(disabled && o.reason);
            var text = reason.querySelector('.cmp-reason-text');
            if (text) text.textContent = o.reason || '';
            var spin = reason.querySelector('.ui-spin');
            if (spin) spin.hidden = !o.busy;
        }
        root.querySelectorAll('.cmp-row button, .cmp-input').forEach(function(control) { control.disabled = !!disabled; });
        if (input) {
            if (disabled && o.reason && reason && reason.id) input.setAttribute('aria-describedby', reason.id);
            else input.removeAttribute('aria-describedby');
        }
        if (!disabled) composer.sync(root, o);
    };
})();
