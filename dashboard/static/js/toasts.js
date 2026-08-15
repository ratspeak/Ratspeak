var TOAST_CLASS_MAP = {
    'toast-error':   'toast-red',
    'toast-warning': 'toast-orange',
    'toast-success': 'toast-green',
    'toast-info':    'toast-blue',
    'toast-progress':'toast-progress',
    'toast-action':  'toast-action'
};

var _activeToasts = new Set();

function createToastStatusIcon(colorClass) {
    var status = document.createElement('span');
    status.className = 'toast-status';
    status.setAttribute('aria-hidden', 'true');

    var ns = 'http://www.w3.org/2000/svg';
    var svg = document.createElementNS(ns, 'svg');
    svg.setAttribute('viewBox', '0 0 20 20');
    svg.setAttribute('fill', 'none');
    svg.setAttribute('stroke', 'currentColor');
    svg.setAttribute('stroke-width', '1.8');
    svg.setAttribute('stroke-linecap', 'round');
    svg.setAttribute('stroke-linejoin', 'round');

    var path = document.createElementNS(ns, 'path');
    if (colorClass === 'toast-green') {
        path.setAttribute('d', 'M5.25 10.25 8.5 13.5 14.75 6.75');
    } else if (colorClass === 'toast-red') {
        path.setAttribute('d', 'm6.5 6.5 7 7m0-7-7 7');
    } else if (colorClass === 'toast-orange' || colorClass === 'toast-yellow') {
        path.setAttribute('d', 'M10 5.75v5.5m0 3v.1');
    } else if (colorClass === 'toast-progress') {
        path.setAttribute('d', 'M5.5 10h.1m4.35 0h.1m4.35 0h.1');
        svg.setAttribute('stroke-width', '2.4');
    } else if (colorClass === 'toast-action') {
        path.setAttribute('d', 'M5.5 10h9m-3.25-3.25L14.5 10l-3.25 3.25');
    } else if (colorClass === 'toast-purple') {
        path.setAttribute('d', 'M10 4.75v10.5M4.75 10h10.5');
    } else {
        path.setAttribute('d', 'M10 8.75v5m0-8.1v.1');
    }

    svg.appendChild(path);
    status.appendChild(svg);
    return status;
}

// onClick is reserved for undo of a just-happened destructive action, or
// navigating to an inbound item. Use rsChoice/rsPrompt for confirm flows.
function showToast(message, colorClass, duration, onClick) {
    colorClass = TOAST_CLASS_MAP[colorClass] || colorClass || '';

    duration = Math.min(duration || 3000, 5000);

    var toastKey = message + '|' + colorClass;
    if (_activeToasts.has(toastKey)) return;
    _activeToasts.add(toastKey);

    var container = document.getElementById('toast-container');
    var toast = document.createElement('div');
    toast.className = 'toast ' + colorClass;
    toast.setAttribute('aria-atomic', 'true');

    if (colorClass === 'toast-red') {
        toast.setAttribute('role', 'alert');
    }

    var dismissed = false;
    function dismissToast() {
        if (dismissed) return;
        dismissed = true;
        _activeToasts.delete(toastKey);
        toast.classList.add('dismiss');
        toast.classList.remove('visible');
        setTimeout(function() { toast.remove(); }, 350);
    }

    toast.appendChild(createToastStatusIcon(colorClass));

    var msgSpan = document.createElement('span');
    msgSpan.className = 'toast-message';
    msgSpan.textContent = message;
    toast.appendChild(msgSpan);

    // Whole toast is the tap target so the click area matches the visual.
    if (onClick) {
        toast.classList.add('toast-actionable');
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
    closeBtn.className = 'toast-close';
    closeBtn.setAttribute('aria-label', 'Dismiss');
    var ns = 'http://www.w3.org/2000/svg';
    var svg = document.createElementNS(ns, 'svg');
    svg.setAttribute('width', '14');
    svg.setAttribute('height', '14');
    svg.setAttribute('viewBox', '0 0 14 14');
    svg.setAttribute('fill', 'none');
    svg.setAttribute('stroke', 'currentColor');
    svg.setAttribute('stroke-width', '2');
    svg.setAttribute('stroke-linecap', 'round');
    svg.setAttribute('aria-hidden', 'true');
    svg.setAttribute('focusable', 'false');
    var line1 = document.createElementNS(ns, 'line');
    line1.setAttribute('x1', '2'); line1.setAttribute('y1', '2');
    line1.setAttribute('x2', '12'); line1.setAttribute('y2', '12');
    var line2 = document.createElementNS(ns, 'line');
    line2.setAttribute('x1', '12'); line2.setAttribute('y1', '2');
    line2.setAttribute('x2', '2'); line2.setAttribute('y2', '12');
    svg.appendChild(line1);
    svg.appendChild(line2);
    closeBtn.appendChild(svg);
    closeBtn.addEventListener('click', function(e) {
        e.stopPropagation();
        dismissToast();
    });
    toast.appendChild(closeBtn);

    RS.gestures.attachSwipe(toast, {
        direction: 'up',
        distanceThreshold: RS.gestures.SWIPE_DISTANCE_TOAST_DISMISS_PX,
        hapticAt: { commit: 'selection' },
        onProgress: function(_dx, dy) {
            if (dy < 0) toast.style.transform = 'translateY(' + dy + 'px)';
        },
        onCommit: dismissToast,
        onCancel: function() { toast.style.transform = ''; }
    });

    container.appendChild(toast);
    requestAnimationFrame(function() {
        toast.classList.add('visible');
    });

    setTimeout(dismissToast, duration);
}

function showCopyConfirmationToast(noun) {
    showToast(noun + ' copied', 'toast-green', 1500);
}

function showRateLimitedToast() {
    showToast('Slow down, announcing too fast', 'toast-orange', 3000);
}

function showPreConditionToast(message) {
    showToast(message, 'toast-orange', 3000);
}
