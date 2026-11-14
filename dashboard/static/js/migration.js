// The one-time network data move: a full-window card, shown only when the move
// is still running a second after it started. The backend owns the progress:
// a `get_network_migration_status` snapshot and `network-migration` events of
// { state, phase, done, total, fraction, elapsed_ms, error: { code, detail, can_skip } }.

var MIGRATION_SHOW_AFTER_MS = 1000;
var MIGRATION_SLOW_MS = 60000;
var MIGRATION_FINISH_HOLD_MS = 800;
var MIGRATION_REASONS = {
    no_space: 'There isn’t enough free space to finish.',
    no_permission: 'Ratspeak can’t save to its data folder.',
    unreadable: 'Some of the old network data couldn’t be read.',
    other: 'Something unexpected stopped it.'
};
var MIGRATION_STEPS = [
    { phase: 'keys', label: 'Moving peer keys' },
    { phase: 'peers', label: 'Updating known peers' }
];

function migrationCount(done, total) {
    var format = typeof Intl !== 'undefined' ? new Intl.NumberFormat() : null;
    function n(value) {
        value = Math.max(0, Math.floor(Number(value) || 0));
        return format ? format.format(value) : String(value);
    }
    return n(done) + ' of ' + n(total);
}

// The bar never moves back, and reads 100 % only once the move has finished.
function migrationFraction(shown, status) {
    if (status && status.state === 'finished') return 1;
    var next = Number(status && status.fraction);
    if (!isFinite(next)) next = 0;
    return Math.max(shown || 0, Math.min(1, Math.max(0, next)));
}

function migrationModel(status, shown) {
    var state = status.state;
    var phase = status.phase === 'peers' ? 'peers' : 'keys';
    var finished = state === 'finished';
    var failed = state === 'failed';
    var running = !finished && !failed;
    var fraction = migrationFraction(shown, status);
    var pct = finished ? 100 : Math.min(99, Math.floor(fraction * 100));
    // A step whose files carry no count (the destination files alone) shows no numbers.
    var count = Number(status.total) > 0 ? migrationCount(status.done, status.total) : '';
    var steps = MIGRATION_STEPS.map(function(step, index) {
        var current = index === (phase === 'peers' ? 1 : 0);
        var past = phase === 'peers' && index === 0;
        if (finished || past) return { label: step.label, status: 'done', meta: 'Done' };
        if (!current) return { label: step.label, status: 'pending', meta: '' };
        if (failed) return { label: step.label, status: 'failed', meta: 'Stopped' };
        return { label: step.label, status: 'active', meta: count };
    });
    var error = status.error || {};
    var label = MIGRATION_STEPS[phase === 'peers' ? 1 : 0].label;
    return {
        state: state,
        phase: phase,
        fraction: fraction,
        pct: pct,
        running: running,
        failed: failed,
        finished: finished,
        title: finished ? 'Update finished' : failed ? 'The update couldn’t finish' : 'Finishing the update',
        lead: finished ? 'Starting the network…'
            : failed ? (MIGRATION_REASONS[error.code] || MIGRATION_REASONS.other) + ' Your messages, contacts, and identity are safe.'
            : 'Ratspeak is moving its network data to a new format. This happens once.',
        steps: steps,
        slow: running && Number(status.elapsed_ms) >= MIGRATION_SLOW_MS,
        canSkip: failed && error.can_skip !== false,
        detail: String(error.detail || ''),
        valueText: finished ? '100 percent. Update finished'
            : pct + ' percent. ' + label + (running && count ? ', ' + count : '') + (failed ? '. Stopped' : '')
    };
}

var _migration = { status: null, shown: 0, visible: false, root: null, showTimer: null, leaving: false, said: null };

function _migrationMark() {
    var mark = document.querySelector('#view-setup .solo-mark');
    return mark ? mark.outerHTML : '';
}

function _migrationMarkup(model) {
    var steps = model.steps.map(function(step, index) {
        return '<li class="progress-step' + (step.status !== 'pending' ? ' is-' + step.status : '') + '" id="migration-step-' + index + '">' +
            '<span class="progress-step-dot" aria-hidden="true"></span><span class="progress-step-label">' + escapeHtml(step.label) + '</span>' +
            '<span class="progress-step-meta">' + escapeHtml(step.meta) + '</span></li>';
    }).join('');
    var icon = RS.kit && RS.kit.icon ? RS.kit.icon('clock') : '';
    return '<div class="solo ui-scroll"><section class="ui-card solo-card" aria-labelledby="migration-title" data-state="' + escapeHtml(model.state) + '">' + _migrationMark() +
        '<h1 class="solo-title" id="migration-title" tabindex="-1">' + escapeHtml(model.title) + '</h1>' +
        '<p class="solo-lead" id="migration-lead">' + escapeHtml(model.lead) + '</p>' +
        '<div class="progress-meter' + (model.running ? ' is-running' : '') + (model.failed ? ' is-failed' : '') + '" id="migration-meter">' +
            '<div class="progress-meter-bar" id="migration-bar" role="progressbar" aria-label="Update progress" aria-valuemin="0" aria-valuemax="100" aria-valuenow="' + model.pct + '" aria-valuetext="' + escapeHtml(model.valueText) + '">' +
                '<span class="progress-meter-fill" id="migration-fill" style="width:' + (model.fraction * 100).toFixed(2) + '%"></span></div>' +
            '<span class="progress-meter-pct" id="migration-pct" aria-hidden="true">' + model.pct + '%</span></div>' +
        '<ol class="progress-steps' + (model.running ? ' is-running' : '') + '" id="migration-steps">' + steps + '</ol>' +
        '<div class="ui-notice migration-note" id="migration-slow"' + (model.slow ? '' : ' hidden') + '>' + icon +
            '<div class="ui-notice-body">Still working. This can take a few minutes when Ratspeak has heard from many peers. If it closes, it picks up where it left off.</div></div>' +
        '<p class="solo-foot" id="migration-foot"' + (model.running ? '' : ' hidden') + '>Keep Ratspeak open until this finishes. Your messages, contacts, and identity aren’t changed.</p>' +
        '<div class="solo-actions" id="migration-actions"' + (model.failed ? '' : ' hidden') + '>' +
            '<button type="button" class="ui-btn ui-btn--primary ui-btn--lg ui-btn--block" id="migration-retry">Try again</button>' +
            '<button type="button" class="ui-btn ui-btn--quiet ui-btn--lg ui-btn--block" id="migration-skip" aria-describedby="migration-skip-note"' + (model.canSkip ? '' : ' hidden') + '>Continue without old data</button>' +
            '<p class="migration-skip" id="migration-skip-note"' + (model.canSkip ? '' : ' hidden') + '>Ratspeak starts with an empty peer list and refills it as peers come online. Some contacts may be unreachable until then.</p>' +
            '<button type="button" class="ui-link" id="migration-copy">Copy details</button></div>' +
        '<div class="ui-sr" role="status" aria-live="polite" id="migration-live"></div>' +
        '<div class="ui-sr" role="alert" id="migration-alert"></div>' +
        '</section></div>';
}

function _migrationRender() {
    var m = _migration;
    if (!m.root || !m.status) return;
    var model = migrationModel(m.status, m.shown);
    m.shown = model.fraction;
    var card = m.root.querySelector('.solo-card');
    var wasFailed = card && card.getAttribute('data-state') === 'failed';
    if (!card || card.getAttribute('data-state') !== model.state) {
        m.root.innerHTML = _migrationMarkup(model);
        _migrationWire();
        var title = m.root.querySelector('.solo-title');
        if (title && (model.failed || !card)) title.focus({ preventScroll: true });
    } else {
        var fill = m.root.querySelector('#migration-fill');
        if (fill) fill.style.width = (model.fraction * 100).toFixed(2) + '%';
        var pct = m.root.querySelector('#migration-pct');
        if (pct) pct.textContent = model.pct + '%';
        var bar = m.root.querySelector('#migration-bar');
        if (bar) {
            bar.setAttribute('aria-valuenow', String(model.pct));
            bar.setAttribute('aria-valuetext', model.valueText);
        }
        model.steps.forEach(function(step, index) {
            var row = m.root.querySelector('#migration-step-' + index);
            if (!row) return;
            row.className = 'progress-step' + (step.status !== 'pending' ? ' is-' + step.status : '');
            row.querySelector('.progress-step-meta').textContent = step.meta;
        });
        var slow = m.root.querySelector('#migration-slow');
        if (slow) slow.hidden = !model.slow;
    }
    _migrationAnnounce(model, wasFailed);
}

// Polite updates only for a phase change, each quarter, the slow note, and the finish.
function _migrationAnnounce(model, wasFailed) {
    var m = _migration;
    var said = m.said || (m.said = { phase: model.phase, quarter: Math.floor(model.pct / 25), slow: false, finished: false });
    var live = m.root.querySelector('#migration-live');
    var text = '';
    if (model.finished && !said.finished) { said.finished = true; text = 'Update finished. Starting the network.'; }
    else if (model.slow && !said.slow) { said.slow = true; text = 'Still working. This can take a few minutes.'; }
    else if (model.phase !== said.phase) { said.phase = model.phase; text = model.steps[1].label + '.'; }
    else if (Math.floor(model.pct / 25) > said.quarter) { said.quarter = Math.floor(model.pct / 25); text = model.pct + ' percent.'; }
    if (text && live) live.textContent = text;
    if (model.failed && !wasFailed) {
        var alert = m.root.querySelector('#migration-alert');
        if (alert) alert.textContent = model.title + '. ' + model.lead;
    }
}

function _migrationWire() {
    var m = _migration;
    var retry = m.root.querySelector('#migration-retry');
    var skip = m.root.querySelector('#migration-skip');
    var copy = m.root.querySelector('#migration-copy');
    function busy(on) {
        if (retry) retry.disabled = on;
        if (skip) skip.disabled = on;
    }
    if (retry) retry.addEventListener('click', function() {
        busy(true);
        // Resumes where it stopped; the events bring the card back to running.
        RS.invoke('retry_network_migration').catch(function() {
            busy(false);
            if (typeof showToast === 'function') showToast('Couldn’t restart the update', 'toast-error', 4000);
        });
    });
    if (skip) skip.addEventListener('click', function() {
        busy(true);
        RS.invoke('skip_network_migration').then(_migrationLeave).catch(function() {
            busy(false);
            if (typeof showToast === 'function') showToast('Couldn’t start without the old data', 'toast-error', 4000);
        });
    });
    if (copy) copy.addEventListener('click', function() {
        var model = migrationModel(m.status, m.shown);
        Promise.resolve(RS.copyText(model.detail || model.lead)).then(function(ok) {
            if (!ok) {
                if (typeof showToast === 'function') showToast('Could not copy details', 'toast-error', 3000);
                return;
            }
            copy.textContent = 'Copied';
            clearTimeout(copy._rsCopiedTimer);
            copy._rsCopiedTimer = setTimeout(function() { copy.textContent = 'Copy details'; }, 2000);
        });
    });
}

function _migrationShow() {
    var m = _migration;
    if (m.visible || !m.status) return;
    m.visible = true;
    m.root = document.createElement('div');
    m.root.className = 'migration-screen';
    m.root.id = 'migration-screen';
    document.body.appendChild(m.root);
    document.body.classList.add('migrating-active');
    _migrationRender();
    var card = m.root.querySelector('.solo-card');
    if (card) card.classList.add('setup-fade-in');
}

function _migrationLeave() {
    var m = _migration;
    if (!m.visible || m.leaving) return;
    m.leaving = true;
    var card = m.root && m.root.querySelector('.solo-card');
    if (card) card.classList.add('setup-fade-out');
    setTimeout(function() {
        if (m.root && m.root.parentNode) m.root.parentNode.removeChild(m.root);
        document.body.classList.remove('migrating-active');
        m.root = null;
        m.visible = false;
        m.leaving = false;
        m.said = null;
    }, 200);
}

function applyNetworkMigration(status) {
    var m = _migration;
    if (!status || typeof status !== 'object' || !status.state) return;
    m.status = status;
    if (!m.visible) {
        // A move that finishes within a second never shows; a failure always does.
        if (status.state === 'finished') return;
        var wait = status.state === 'failed' ? 0 : Math.max(0, MIGRATION_SHOW_AFTER_MS - (Number(status.elapsed_ms) || 0));
        clearTimeout(m.showTimer);
        if (wait === 0) _migrationShow();
        else m.showTimer = setTimeout(function() {
            if (m.status && m.status.state !== 'finished') _migrationShow();
        }, wait);
        return;
    }
    _migrationRender();
    if (status.state === 'finished') setTimeout(_migrationLeave, MIGRATION_FINISH_HOLD_MS);
}

(function() {
    if (typeof RS === 'undefined' || typeof RS.invoke !== 'function') return;
    // Listen first: the snapshot covers events sent before the page loaded.
    if (typeof RS.listen === 'function') {
        Promise.resolve(RS.listen('network-migration', applyNetworkMigration)).catch(function() {});
    }
    RS.invoke('get_network_migration_status').then(applyNetworkMigration).catch(function() {});
})();
