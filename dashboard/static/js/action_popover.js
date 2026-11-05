// Floating menu anchored to a trigger element (desktop counterpart to mobile FAB).

(function() {
    var _activePopover = null;

    function close() {
        if (!_activePopover) return;
        var p = _activePopover;
        _activePopover = null;
        p.popover.classList.remove('open');
        document.removeEventListener('keydown', p.onKey, true);
        window.removeEventListener('resize', p.onReflow);
        window.removeEventListener('scroll', p.onReflow, true);
        setTimeout(function() {
            if (p.scrim.parentNode) p.scrim.remove();
            if (p.popover.parentNode) p.popover.remove();
        }, 130);
        if (typeof p.onClose === 'function') {
            try { p.onClose(); } catch (_) {}
        }
    }

    function position(popover, trigger) {
        var r = trigger.getBoundingClientRect();
        var pw = popover.offsetWidth;
        var ph = popover.offsetHeight;
        var vw = window.innerWidth;
        var vh = window.innerHeight;
        var margin = 8;
        var safe = safeAreaInsets();
        var minTop = safe.top + margin;
        var minLeft = safe.left + margin;
        var maxRight = vw - safe.right - margin;
        var maxBottom = vh - safe.bottom - margin;

        var top = r.top - ph - margin;
        var origin = 'bottom left';

        if (top < minTop) {
            top = r.bottom + margin;
            origin = 'top left';
        }

        var left = r.left + (r.width / 2) - 24;
        if (left + pw > maxRight) {
            left = maxRight - pw;
            origin = origin.replace('left', 'right');
        }
        if (left < minLeft) left = minLeft;
        if (top + ph > maxBottom) top = maxBottom - ph;

        popover.style.top = top + 'px';
        popover.style.left = left + 'px';
        popover.style.setProperty('--popover-origin', origin);
    }

    // items: [{ label, icon: '<svg>', onSelect: function, disabled?, danger? }]
    // opts: { onClose?: function } — fires after close animation regardless of dismissal path.
    window.actionPopover = function(trigger, items, opts) {
        if (!trigger || !items || !items.length) return;
        if (_activePopover && _activePopover.trigger === trigger) {
            close();
            return;
        }
        close();
        opts = opts || {};

        var scrim = document.createElement('div');
        scrim.className = 'action-popover-scrim';
        // preventDefault on mousedown/touchstart keeps any prior text-input focused
        // (and the soft keyboard up). preventDefault on touchstart also suppresses
        // the synthetic click on iOS/Android, so dismissal must dispatch from touchend.
        var scrimDismissed = false;
        function _dismissScrim(e) {
            if (scrimDismissed) return;
            scrimDismissed = true;
            if (e) { e.preventDefault(); e.stopPropagation(); }
            close();
        }
        scrim.addEventListener('mousedown', function(e) { e.preventDefault(); });
        scrim.addEventListener('touchstart', function(e) { e.preventDefault(); }, { passive: false });
        scrim.addEventListener('touchend', _dismissScrim);
        scrim.addEventListener('click', _dismissScrim);
        scrim.addEventListener('contextmenu', function(e) { e.preventDefault(); _dismissScrim(); });

        var popover = document.createElement('div');
        popover.className = 'action-popover';
        popover.setAttribute('role', 'menu');

        items.forEach(function(item) {
            if (item.separator) {
                var separator = document.createElement('div');
                separator.className = 'action-popover-separator';
                separator.setAttribute('role', 'separator');
                popover.appendChild(separator);
                return;
            }
            var btn = document.createElement('button');
            btn.type = 'button';
            btn.className = 'action-popover-item' + (item.danger ? ' action-popover-item--danger' : '');
            btn.setAttribute('role', 'menuitem');

            var icon = document.createElement('span');
            icon.className = 'action-popover-item-icon';
            icon.innerHTML = item.icon || '';

            var label = document.createElement('span');
            label.className = 'action-popover-item-label';
            label.textContent = item.label;

            btn.appendChild(icon);
            btn.appendChild(label);

            if (item.disabled) {
                btn.disabled = true;
                btn.classList.add('disabled');
            } else {
                // preventDefault on mousedown/touchstart keeps any prior text-input
                // focused (and the soft keyboard up). preventDefault on touchstart
                // also suppresses the synthetic click event on Android/iOS, so the
                // selection must dispatch from touchend in addition to click.
                var activated = false;
                function _activate(e) {
                    if (activated) return;
                    activated = true;
                    if (e) { e.preventDefault(); e.stopPropagation(); }
                    close();
                    if (typeof item.onSelect === 'function') {
                        setTimeout(item.onSelect, 0);
                    }
                }
                btn.addEventListener('mousedown', function(e) { e.preventDefault(); });
                btn.addEventListener('touchstart', function(e) { e.preventDefault(); }, { passive: false });
                btn.addEventListener('touchend', function(e) {
                    // Slide-off cancel: only fire if the lift point is still over this button.
                    var t = (e.changedTouches && e.changedTouches[0]) || null;
                    if (t) {
                        var hit = document.elementFromPoint(t.clientX, t.clientY);
                        if (hit !== btn && !btn.contains(hit)) return;
                    }
                    _activate(e);
                });
                btn.addEventListener('click', _activate);
            }

            popover.appendChild(btn);
        });

        document.body.appendChild(scrim);
        document.body.appendChild(popover);

        function onKey(e) {
            if (e.key === 'Escape') {
                e.stopPropagation();
                close();
            }
        }
        function onReflow() { if (_activePopover) position(popover, trigger); }

        _activePopover = {
            trigger: trigger,
            popover: popover,
            scrim: scrim,
            onKey: onKey,
            onReflow: onReflow,
            onClose: opts.onClose || null
        };

        document.addEventListener('keydown', onKey, true);
        window.addEventListener('resize', onReflow);
        window.addEventListener('scroll', onReflow, true);

        position(popover, trigger);
        requestAnimationFrame(function() { popover.classList.add('open'); });

        var first = popover.querySelector('.action-popover-item');
        if (first && !window.matchMedia('(hover: none)').matches) first.focus();
    };

    window.closeActionPopover = close;
})();
