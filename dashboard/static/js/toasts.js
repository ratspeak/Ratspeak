// Semantic intents map to the kit toast tones. Unknown or legacy colour names read as info.
var TOAST_CLASS_MAP = {
    'toast-error':   'error',
    'toast-warning': 'warning',
    'toast-success': 'success',
    'toast-info':    'info',
    'toast-progress':'progress',
    'toast-action':  'action'
};

// The bare tone glyph: a check, a warning, an error, information, or something received.
var TOAST_GLYPHS = {
    error: 'x-circle',
    warning: 'alert',
    success: 'check-circle',
    info: 'info',
    action: 'inbox'
};

var _activeToasts = new Set();

function toastDeveloperDetailsEnabled() {
    return window.__RATSPEAK_DIAGNOSTICS__ === true ||
        (typeof window.ratspeakDeveloperModeEnabled === 'function' &&
            window.ratspeakDeveloperModeEnabled());
}

function toastErrorCopy(error, fallback) {
    var base = String(fallback || 'Action failed');
    var detail = error && error.message
        ? String(error.message)
        : (typeof error === 'string' ? error : '');
    if (!detail || detail === base) return base;

    // Validation/auth/conflict messages are deliberately written for users.
    // Uncoded client errors are likewise authored at the interaction site.
    var safeCodes = ['bad_request', 'unauthorized', 'forbidden', 'not_found', 'conflict', 'locked'];
    var code = error && error.code ? String(error.code) : '';
    if (!code || safeCodes.indexOf(code) !== -1) return detail;

    // Internal/database/runtime detail is useful only in an explicit support
    // session. Normal product UI keeps the actionable fallback.
    return toastDeveloperDetailsEnabled() ? base + ' — ' + detail : base;
}

// Toasts are user interruptions. Use them only when the user must correct an
// action, when an otherwise invisible action needs confirmation, or when an
// asynchronous inbound/background event deserves attention. Routine state
// changes already rendered in the interface must stay inline. Diagnostics
// belong in RS.diag/Activity, not here. onClick is reserved for undo of a
// just-happened destructive action, navigating to an inbound item, or copying
// an error's details, named by actionLabel; use rsChoice/rsPrompt for
// confirmation flows.
function showToast(message, colorClass, duration, onClick, actionLabel) {
    var tone = TOAST_CLASS_MAP[colorClass] || 'info';

    duration = Math.min(duration || 3000, 5000);

    var toastKey = message + '|' + tone;
    if (_activeToasts.has(toastKey)) return;
    _activeToasts.add(toastKey);

    var container = document.getElementById('toast-container');
    var toast = document.createElement('div');
    // The kit toast: info and progress share the info tone; an inbound action uses it too.
    var kitTone = tone === 'error' || tone === 'warning' || tone === 'success' ? ' ui-toast--' + tone : '';
    toast.className = 'ui-toast' + kitTone + ' is-entering';
    toast.setAttribute('aria-atomic', 'true');

    if (tone === 'error') {
        toast.setAttribute('role', 'alert');
    }

    var dismissed = false;
    function dismissToast() {
        if (dismissed) return;
        dismissed = true;
        _activeToasts.delete(toastKey);
        toast.classList.add('is-leaving');
        setTimeout(function() { toast.remove(); }, 240);
    }

    // Work in progress turns; every other tone is a still glyph.
    toast.insertAdjacentHTML('beforeend', tone === 'progress'
        ? '<span class="ui-spin" aria-hidden="true"></span>'
        : RS.kit.icon(TOAST_GLYPHS[tone]));

    var msgSpan = document.createElement('span');
    msgSpan.className = 'msg';
    msgSpan.textContent = message;
    toast.appendChild(msgSpan);

    if (onClick && actionLabel) {
        // A named action (copying an error's details) is its own Link, so the card stays readable.
        var link = document.createElement('button');
        link.type = 'button';
        link.className = 'ui-link';
        link.textContent = actionLabel;
        link.addEventListener('click', function() {
            dismissToast();
            onClick();
        });
        toast.appendChild(link);
    } else if (onClick) {
        // An inbound item: the whole card opens it, and "Open" says so.
        toast.classList.add('is-actionable');
        var open = document.createElement('span');
        open.className = 'ui-link';
        open.setAttribute('aria-hidden', 'true');
        open.textContent = 'Open';
        toast.appendChild(open);
        var actionBtn = document.createElement('button');
        actionBtn.type = 'button';
        actionBtn.className = 'toast-action-target';
        actionBtn.setAttribute('aria-label', 'Open: ' + message);
        actionBtn.addEventListener('click', function() {
            dismissToast();
            onClick();
        });
        toast.appendChild(actionBtn);
    }

    var closeBtn = document.createElement('button');
    closeBtn.type = 'button';
    closeBtn.className = 'ui-icon-btn ui-icon-btn--sm';
    closeBtn.setAttribute('aria-label', 'Dismiss');
    closeBtn.title = 'Dismiss';
    closeBtn.innerHTML = RS.kit.icon('x');
    closeBtn.addEventListener('click', function(e) {
        e.stopPropagation();
        dismissToast();
    });
    toast.appendChild(closeBtn);

    // Toasts rise from the bottom bar, so a swipe down puts one away.
    RS.gestures.attachSwipe(toast, {
        direction: 'down',
        distanceThreshold: RS.gestures.SWIPE_DISTANCE_TOAST_DISMISS_PX,
        hapticAt: { commit: 'selection' },
        onProgress: function(_dx, dy) {
            if (dy > 0) toast.style.transform = 'translateY(' + dy + 'px)';
        },
        onCommit: dismissToast,
        onCancel: function() { toast.style.transform = ''; }
    });

    container.appendChild(toast);
    requestAnimationFrame(function() {
        requestAnimationFrame(function() { toast.classList.remove('is-entering'); });
    });

    setTimeout(dismissToast, duration);
}

function showCopyConfirmationToast(noun) {
    showToast(noun + ' copied', 'toast-success', 1500);
}

// Matches RS.copySecret: a copied key or phrase leaves the clipboard after a minute.
function showSecretCopiedToast(noun) {
    showToast(noun + ' copied. It clears from the clipboard in a minute.', 'toast-success', 3000);
}

function showRateLimitedToast() {
    showToast('Wait a moment before announcing again', 'toast-warning', 3000);
}

function showPreConditionToast(message) {
    showToast(message, 'toast-warning', 3000);
}
