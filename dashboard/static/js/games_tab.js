(function() {
    'use strict';

    var _allSessions = [];
    var _activeFilter = 'all';
    var _selectedSessionId = null;
    var _contactNameCache = {};
    var _gameEventsReady = false;
    var _animatingCell = -1;
    var _animatingCellExpiry = 0;
    // Pre-mutation snapshot per session_id; restored on game_action_result failure.
    var _optimisticBackup = {};
    var _celebratedWins = {};
    var _actionInFlight = {};
    var _manifestsById = {};

    var WIN_LINES = [
        [0,1,2],[3,4,5],[6,7,8],
        [0,3,6],[1,4,7],[2,5,8],
        [0,4,8],[2,4,6]
    ];

    function _getMyHash(session) {
        if (session && session.my_lxmf_hash) return session.my_lxmf_hash;
        if (typeof lxmfIdentity !== 'undefined' && lxmfIdentity && lxmfIdentity.hash) {
            return lxmfIdentity.hash;
        }
        return '';
    }

    function _getContacts() {
        return (typeof lxmfContacts !== 'undefined') ? lxmfContacts : [];
    }

    function _contactName(hash) {
        if (!hash) return 'Unknown';
        if (_contactNameCache[hash]) return _contactNameCache[hash];
        var contacts = _getContacts();
        for (var i = 0; i < contacts.length; i++) {
            if (contacts[i].hash === hash) {
                var contactName = contacts[i].display_name ? contacts[i].display_name.trim() : '';
                if (contactName) {
                    _contactNameCache[hash] = contactName;
                    return _contactNameCache[hash];
                }
                break;
            }
        }
        if (typeof PeersCache !== 'undefined' && PeersCache && typeof PeersCache.get === 'function') {
            var peer = PeersCache.get(hash);
            var peerName = peer && peer.display_name ? peer.display_name.trim() : '';
            if (peerName) {
                _contactNameCache[hash] = peerName;
                return _contactNameCache[hash];
            }
        }
        return shortHash(hash, 8, 4);
    }

    function _isMe(session, hash) {
        var myHash = _getMyHash(session);
        return myHash && hash === myHash;
    }

    function _sessionValue(session, key, fallback) {
        return RS.games.state.value(session, key, fallback);
    }

    function _drawOfferOwner(session) {
        return _sessionValue(session, 'draw_offered_by', '');
    }

    function _appId(session) {
        return (session && (session.app_id || session.game)) || '';
    }

    function _gameView(appId) {
        return RS.games && RS.games.views ? RS.games.views.get(appId) : null;
    }

    function _gameViewContext(session, root) {
        return {
            root: root || document,
            isMe: function(hash) { return _isMe(session, hash); },
            myHash: function() { return _getMyHash(session); },
            contactName: _contactName,
            sendMove: function(payload, optimistic) {
                return _sendGameViewMove(session, payload, optimistic);
            },
        };
    }

    function _isViewingSession(sessionId) {
        if (!sessionId || _selectedSessionId !== sessionId) return false;
        if (typeof currentView === 'undefined' || currentView !== 'games') return false;
        if (typeof isCompactLayout === 'function' && isCompactLayout()) {
            if (typeof RS === 'undefined' || !RS.viewStack || typeof RS.viewStack.top !== 'function') {
                return false;
            }
            var top = RS.viewStack.top();
            return !!(top && top.viewId === 'game-detail' &&
                (!top.meta || !top.meta.sessionId || top.meta.sessionId === sessionId));
        }
        return true;
    }

    function _markSessionReadLocal(sessionId, options) {
        if (!sessionId) return false;
        options = options || {};
        var changed = false;
        for (var i = 0; i < _allSessions.length; i++) {
            if (_allSessions[i].game_id === sessionId && _allSessions[i].unread > 0) {
                _allSessions[i].unread = 0;
                changed = true;
                break;
            }
        }
        if ((changed || options.force) && typeof RS !== 'undefined' && RS.invoke) {
            RS.invoke('mark_game_read', { sessionId: sessionId }).catch(function() {});
        }
        if (changed && options.render !== false) {
            renderSessionList();
            updateGamesBadge();
        }
        return changed;
    }

    function _markViewedSessionRead(options) {
        if (!_isViewingSession(_selectedSessionId)) return false;
        return _markSessionReadLocal(_selectedSessionId, options);
    }

    function _celebrationOptions(session) {
        var appId = _appId(session);
        var view = _gameView(appId);
        var opts = view && view.celebrationOptions
            ? view.celebrationOptions(session, _gameViewContext(session))
            : { count: 48, duration: 1600 };
        var target = view && view.boardSelector
            ? document.querySelector(view.boardSelector)
            : null;
        if (target) {
            var rect = target.getBoundingClientRect();
            if (rect.width > 0 && rect.height > 0) {
                opts.x = rect.left + rect.width / 2;
                opts.y = rect.top + rect.height / 2.4;
            }
        }
        return opts;
    }

    function _maybeCelebrateWin(session) {
        if (!session || session.status !== 'completed' ||
                !_isMe(session, _sessionValue(session, 'winner', ''))) return;
        if (!session.game_id || _celebratedWins[session.game_id]) return;

        _celebratedWins[session.game_id] = true;
        if (session.game_id === _selectedSessionId && typeof haptic === 'function') {
            haptic('success');
        }
        if (typeof currentView !== 'undefined' && currentView === 'games' && typeof rsConfetti === 'function') {
            rsConfetti(_celebrationOptions(session));
        }
    }

    function _isSendingDeliveryState(state) {
        return state === 'sending' ||
            state === 'link_establishing' ||
            state === 'sending_via_link' ||
            state === 'reusing_direct_link' ||
            state === 'reusing_backchannel';
    }

    function _activeMoveDeliveryText(state) {
        if (_isSendingDeliveryState(state) || state === 'pending' || state === 'routing') {
            return 'Sending move…';
        }
        if (state === 'propagating') return 'Storing move in Offline Inbox…';
        if (state === 'sent') return 'Move sent';
        if (state === 'failed') return 'Move failed — tap Resend';
        return '';
    }

    function _statusText(session) {
        var status = session.status;

        if (status === 'pending') {
            if (_isMe(session, session.challenger)) {
                // LXMF Direct's MAX_DELIVERY_ATTEMPTS=5 handles transient
                // wire loss; `failed` here means the transport gave up.
                // Resend is exposed via the `Resend last move` button.
                if (_isSendingDeliveryState(session.delivery_state)) return 'Sending…';
                switch (session.delivery_state) {
                    case 'sent':
                    case 'routing':          return 'Sent';
                    case 'propagating':      return 'Storing in Offline Inbox…';
                    case 'propagated':       return 'Stored in Offline Inbox';
                    case 'delivered':        return 'Waiting...';
                    case 'failed':           return 'Failed — tap Resend';
                    default:                 return 'Waiting...';
                }
            }
            return 'Challenge!';
        }
        if (status === 'declined') {
            if (session.cancelled_by_initiator) {
                return _isMe(session, session.challenger) ? 'Cancelled' : 'Challenge cancelled';
            }
            return _isMe(session, session.challenger) ? 'Declined' : 'You declined';
        }
        if (status === 'expired') return 'Expired';
        if (status === 'completed') {
            var t = _sessionValue(session, 'terminal', '');
            var winner = _sessionValue(session, 'winner', '');
            if (t === 'draw') return 'Draw';
            if (t === 'resign') {
                return _isMe(session, winner) ? 'They resigned' : 'You resigned';
            }
            if (_isMe(session, winner)) return 'You won!';
            if (winner) return 'You lost!';
            return 'Completed';
        }
        if (status === 'active') {
            // In-flight/failed outbound move overrides the "their turn" label.
            var deliveryText = _activeMoveDeliveryText(session.delivery_state);
            if (deliveryText) return deliveryText;
            if (_sessionValue(session, 'draw_offered', false)) {
                return _isMe(session, _drawOfferOwner(session))
                    ? 'Draw offer sent'
                    : 'Draw offered';
            }
            var view = _gameView(_appId(session));
            if (view && view.activeStatusText) {
                var customStatus = view.activeStatusText(session, _gameViewContext(session));
                if (customStatus) return customStatus;
            }
            var turn = _sessionValue(session, 'turn', '');
            if (_isMe(session, turn)) return 'Your turn';
            if (turn) {
                var name = _contactName(session.contact_hash) || 'Opponent';
                return name + '\u2019s turn';
            }
            return 'Active';
        }
        return status;
    }

    function _statusClass(session) {
        var status = session.status;

        if (status === 'pending') {
            if (_isMe(session, session.challenger)) {
                if (session.delivery_state === 'failed') return 'status-lost';
                return 'status-waiting';
            }
            return 'status-challenge';
        }
        if (status === 'active') {
            if (session.delivery_state === 'failed') return 'status-lost';
            if (_activeMoveDeliveryText(session.delivery_state)) return 'status-waiting';
            if (_sessionValue(session, 'draw_offered', false)) {
                return _isMe(session, _drawOfferOwner(session))
                    ? 'status-waiting'
                    : 'status-challenge';
            }
            return _isMe(session, _sessionValue(session, 'turn', ''))
                ? 'status-your-turn'
                : 'status-their-turn';
        }
        if (status === 'completed') {
            if (_isMe(session, _sessionValue(session, 'winner', ''))) return 'status-won';
            if (_sessionValue(session, 'terminal', '') === 'draw') return 'status-draw';
            return 'status-lost';
        }
        return 'status-muted';
    }

    function _gameIcon(appId) {
        var view = _gameView(appId);
        if (view) return view.icon;
        var manifest = _manifestsById[appId];
        var icon = manifest && manifest.icon;
        if (icon && icon.length <= 2) return icon;
        return '?';
    }

    function _gameIconMarkup(appId) {
        if (appId === 'four_in_a_row') {
            return '<span class="games-four-icon-mark" aria-hidden="true">' +
                '<span></span><span></span><span></span><span></span>' +
            '</span>';
        }
        return escapeHtml(_gameIcon(appId));
    }

    function _gameName(appId) {
        var manifest = _manifestsById[appId];
        if (manifest && manifest.display_name) return manifest.display_name;
        var view = _gameView(appId);
        if (view && view.displayName) return view.displayName;
        if (appId === 'ttt') return 'Tic-Tac-Toe';
        if (appId === 'chess') return 'Chess';
        return appId || 'Unknown';
    }

    function _loadGameManifests() {
        return RS.invoke('get_available_games').then(function(manifests) {
            if (!Array.isArray(manifests)) return;
            var next = {};
            for (var i = 0; i < manifests.length; i++) {
                var manifest = manifests[i];
                if (manifest && manifest.app_id) next[manifest.app_id] = manifest;
            }
            _manifestsById = next;
        }).catch(function() {});
    }

    function _beginSessionAction(sessionId) {
        if (!sessionId || _actionInFlight[sessionId]) return false;
        _actionInFlight[sessionId] = true;
        return true;
    }

    function _finishSessionAction(sessionId) {
        if (sessionId) delete _actionInFlight[sessionId];
    }

    function _filterSessions() {
        if (_activeFilter === 'all') return _allSessions;
        return _allSessions.filter(function(s) {
            var status = s.status;
            if (_activeFilter === 'active') return status === 'active';
            if (_activeFilter === 'pending') return status === 'pending';
            if (_activeFilter === 'completed') return status === 'completed' || status === 'declined' || status === 'expired';
            return true;
        });
    }

    function _findSession(sessionId) {
        for (var i = 0; i < _allSessions.length; i++) {
            if (_allSessions[i].game_id === sessionId) return _allSessions[i];
        }
        return null;
    }

    function _canDeleteSession(session) {
        return !!session && (session.status === 'completed' ||
            session.status === 'declined' || session.status === 'expired');
    }

    function renderSessionList() {
        var container = document.getElementById('games-session-list');
        if (!container) return;

        var filtered = _filterSessions();
        if (filtered.length === 0) {
            container.innerHTML = '<div class="empty-state">' +
                '<svg class="empty-state-svg" width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="2" y="4" width="20" height="16" rx="2"/><line x1="8" y1="2" x2="8" y2="6"/><line x1="16" y1="2" x2="16" y2="6"/><circle cx="8" cy="12" r="2"/><circle cx="16" cy="12" r="2"/><path d="M10 12h4"/></svg>' +
                '<span class="empty-state-primary">No games yet</span>' +
                '<span class="empty-state-hint">Start a new game with a contact</span>' +
            '</div>';
            return;
        }

        var html = '';
        for (var i = 0; i < filtered.length; i++) {
            var s = filtered[i];
            var isActive = s.game_id === _selectedSessionId;
            var isViewing = _isViewingSession(s.game_id);
            var classes = 'games-session-row';
            if (isActive) classes += ' active';
            if (s.unread > 0 && !isViewing) classes += ' unread';

            var appId = _appId(s);
            html += '<div class="' + classes + ' game-row-' + escapeHtml(appId || 'unknown') + '" data-session-id="' + escapeHtml(s.game_id) + '" role="button" tabindex="0">' +
                '<div class="games-session-icon">' + _gameIconMarkup(s.app_id || s.game) + '</div>' +
                '<div class="games-session-info">' +
                    '<div class="games-session-name">' + ratspeakDisplayNameHtml(_contactName(s.contact_hash), s.contact_hash) + '</div>' +
                    '<div class="games-session-meta">' +
                        '<span class="games-session-game">' + escapeHtml(_gameName(appId)) + '</span>' +
                        '<span class="games-session-status ' + _statusClass(s) + '">' + escapeHtml(_statusText(s)) + '</span>' +
                    '</div>' +
                '</div>' +
                '<div class="games-session-time">' + escapeHtml(RS.relativeTime(s.updated_at || s.last_action_at)) + '</div>' +
                (_canDeleteSession(s) ?
                    '<button type="button" class="games-session-delete" aria-label="Remove game from history" title="Remove from history">' +
                        '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 6h18"/><path d="M8 6V4h8v2"/><path d="M19 6l-1 14H6L5 6"/><path d="M10 11v5"/><path d="M14 11v5"/></svg>' +
                    '</button>' : '') +
            '</div>';
        }
        container.innerHTML = html;

        var rows = container.querySelectorAll('.games-session-row');
        for (var j = 0; j < rows.length; j++) {
            _bindSessionRow(rows[j]);
        }
    }

    function _bindSessionRow(row) {
        var sessionId = row.getAttribute('data-session-id');

        row.addEventListener('click', function(e) {
            if (e.target && e.target.closest && e.target.closest('.games-session-delete')) return;
            selectSession(sessionId);
        });
        row.addEventListener('keydown', function(e) {
            if (e.key !== 'Enter' && e.key !== ' ') return;
            if (e.target && e.target.closest && e.target.closest('.games-session-delete')) return;
            e.preventDefault();
            selectSession(sessionId);
        });

        var deleteBtn = row.querySelector('.games-session-delete');
        if (deleteBtn) {
            deleteBtn.addEventListener('click', function(e) {
                e.stopPropagation();
                _confirmDeleteSession(sessionId, false);
            });
        }

        if (isMobile() && _canDeleteSession(_findSession(sessionId))) {
            var firedRecently = false;
            RS.gestures.attachLongPress(row, {
                duration: RS.gestures.LONG_PRESS_GAMES_ROW_MS,
                moveCancelPx: RS.gestures.LONG_PRESS_MOVE_CANCEL_PX,
                hapticStages: [{ at: 1.0, level: 'medium' }],
                onFire: function() {
                    firedRecently = true;
                    _confirmDeleteSession(sessionId, true);
                }
            });
            // Swallow the synthetic click after a long-press.
            row.addEventListener('click', function(e) {
                if (firedRecently) {
                    firedRecently = false;
                    if (e.cancelable) e.preventDefault();
                    e.stopPropagation();
                }
            }, true);
        }
    }

    function _confirmDeleteSession(sessionId, mobile) {
        if (!sessionId) return;
        if (!_canDeleteSession(_findSession(sessionId))) {
            if (typeof showToast === 'function') {
                showToast('Finish the game before removing it', 'toast-red', 3000);
            }
            return;
        }
        if (mobile) {
            _showDeleteSheet(sessionId);
        } else if (typeof rsConfirm === 'function') {
            rsConfirm({
                title: 'Remove game?',
                message: 'Remove this game from your history?\nThis only affects your local list — the other player keeps their copy.',
                confirmText: 'Remove',
                cancelText: 'Cancel',
                danger: true,
            }).then(function(ok) {
                if (ok) _deleteSession(sessionId);
            });
        } else if (typeof showToast === 'function') {
            showToast('Confirmation dialog unavailable', 'toast-red', 3000);
        }
    }

    function _showDeleteSheet(sessionId) {
        if (typeof rsConfirm !== 'function') return;
        rsConfirm({
            title: 'Remove game?',
            message: 'Remove this game from your history? This only affects your local list.',
            confirmText: 'Remove',
            danger: true
        }).then(function(ok) {
            if (!ok) return;
            if (typeof haptic === 'function') haptic('warning');
            _deleteSession(sessionId);
        });
    }

    function _deleteSession(sessionId) {
        RS.invoke('delete_game_session', { sessionId: sessionId }).then(function() {
            _removeSessionLocal(sessionId);
        }).catch(function() {
            if (typeof showToast === 'function') {
                showToast('Game could not be removed', 'toast-red', 3000);
            }
        });
    }

    function _removeSessionLocal(sessionId) {
        var filtered = [];
        for (var i = 0; i < _allSessions.length; i++) {
            if (_allSessions[i].game_id !== sessionId) filtered.push(_allSessions[i]);
        }
        _allSessions = filtered;
        delete _celebratedWins[sessionId];
        if (_selectedSessionId === sessionId) {
            _selectedSessionId = null;
            if (isCompactLayout() &&
                RS.viewStack.top() && RS.viewStack.top().viewId === 'game-detail') {
                RS.viewStack.pop();
            }
            renderDetail();
        }
        renderSessionList();
        updateGamesBadge();
    }

    function selectSession(sessionId) {
        _selectedSessionId = sessionId;
        _markSessionReadLocal(sessionId, { render: false, force: true });

        renderSessionList();
        renderDetail();
        updateGamesBadge();

        if (isCompactLayout()) {
            RS.viewStack.push('game-detail', { meta: { sessionId: sessionId } });
            history.pushState({ view: 'games', detail: true }, '', '#games');
        }
    }

    function _getSelectedSession() {
        return _selectedSessionId ? _findSession(_selectedSessionId) : null;
    }

    function _renderDetailMeta(session) {
        var chips = [];
        var moveCount = parseInt(_sessionValue(session, 'move_count', ''), 10);
        if (!isNaN(moveCount) && moveCount > 0) chips.push('Move ' + moveCount);

        var view = _gameView(_appId(session));
        if (view && view.detailChips) {
            var gameChips = view.detailChips(session, _gameViewContext(session));
            if (Array.isArray(gameChips)) chips = chips.concat(gameChips);
        }

        if (_isSendingDeliveryState(session.delivery_state)) chips.push('Sending');
        if (session.delivery_state === 'propagating') chips.push('Offline Inbox');
        if (session.delivery_state === 'propagated') chips.push('Stored');
        if (session.delivery_state === 'failed') chips.push('Retry needed');

        if (chips.length === 0) return '';
        return '<div class="games-detail-meta">' + chips.map(function(chip) {
            return '<span class="games-detail-chip">' + escapeHtml(chip) + '</span>';
        }).join('') + '</div>';
    }

    function renderDetail() {
        var panel = document.getElementById('games-detail');
        if (!panel) return;

        var session = _getSelectedSession();
        if (!session) {
            panel.removeAttribute('data-game');
            panel.innerHTML =
                '<div class="empty-state games-empty-detail">' +
                    '<svg class="empty-state-svg" width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="2" y="4" width="20" height="16" rx="2"/><line x1="8" y1="2" x2="8" y2="6"/><line x1="16" y1="2" x2="16" y2="6"/><circle cx="8" cy="12" r="2"/><circle cx="16" cy="12" r="2"/><path d="M10 12h4"/></svg>' +
                    '<span class="empty-state-primary">Select a game to play</span>' +
                    '<span class="empty-state-hint">or start a new game with a contact</span>' +
                    '<button class="nr-btn nr-btn-primary games-empty-new-btn" type="button">New game</button>' +
                '</div>';
            var emptyBtn = panel.querySelector('.games-empty-new-btn');
            if (emptyBtn) emptyBtn.addEventListener('click', showNewGameDialog);
            return;
        }

        var appId = _appId(session);
        var gameView = _gameView(appId);
        panel.setAttribute('data-game', appId);
        var status = session.status;
        var statusTxt = _statusText(session);
        var statusCls = _statusClass(session);
        var themeClass = gameView ? gameView.themeClass : 'games-theme-unknown';

        var html = '';

        html += '<button class="mobile-back-btn games-back-btn" aria-label="Back to games list">' +
            '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="15 18 9 12 15 6"/></svg>' +
        '</button>';

        html += '<div class="games-detail-header ' + themeClass + '">' +
            '<div class="games-detail-heading">' +
                '<span class="games-detail-icon">' + _gameIconMarkup(appId) + '</span>' +
                '<span class="games-detail-copy">' +
                    '<span class="games-detail-title">' + escapeHtml(_gameName(appId)) + '</span>' +
                    '<span class="games-detail-vs">vs ' + ratspeakDisplayNameHtml(_contactName(session.contact_hash), session.contact_hash) + '</span>' +
                '</span>' +
            '</div>' +
            _renderDetailMeta(session) +
        '</div>';

        html += '<div class="games-detail-status ' + statusCls + '">' + escapeHtml(statusTxt) + '</div>';

        // Direct's MAX_DELIVERY_ATTEMPTS=5 covers transient wire loss; if the
        // session still ended up `failed` the user can manually retransmit.
        // Sends the same envelope (preserved on the action row), so app-layer
        // sequencing handles the rare case where it actually arrived.
        if (session.delivery_state === 'failed') {
            html += '<button class="nr-btn nr-btn-ghost games-resend-btn" type="button">Resend last move</button>';
        }

        html += '<div class="games-detail-board games-board-' + escapeHtml(appId || 'unknown') + '">';
        if (gameView) {
            html += gameView.renderBoard(session, _gameViewContext(session, panel));
        } else {
            html += '<div class="empty-state-primary">Unsupported game type</div>';
        }
        html += '</div>';

        html += '<div class="games-detail-controls">';
        html += _renderControls(session);
        html += '</div>';

        panel.innerHTML = html;

        // Recent cell keeps its animation class across re-renders.
        if (_animatingCell >= 0 && Date.now() < _animatingCellExpiry) {
            var animCell = panel.querySelector('.ttt-cell[data-cell-index="' + _animatingCell + '"]');
            if (animCell) animCell.classList.add('just-placed');
        }

        var backBtn = panel.querySelector('.games-back-btn');
        if (backBtn) {
            backBtn.addEventListener('click', function() {
                RS.viewStack.pop();
            });
        }

        var resendBtn = panel.querySelector('.games-resend-btn');
        if (resendBtn) {
            resendBtn.addEventListener('click', function() {
                if (typeof haptic === 'function') haptic('selection');
                resendBtn.disabled = true;
                resendBtn.textContent = 'Resending…';
                session.delivery_state = 'sending';
                renderSessionList();
                renderDetail();
                RS.invoke('resend_last_game_action', {
                    args: { session_id: session.game_id }
                }).catch(function(err) {
                    session.delivery_state = 'failed';
                    renderSessionList();
                    renderDetail();
                    if (typeof showToast === 'function') {
                        var msg = (err && err.message) || 'Resend failed';
                        showToast(msg, 'toast-red', 4000);
                    }
                });
            });
        }

        _bindControlEvents(session);
        if (gameView) gameView.bindBoard(session, _gameViewContext(session, panel));
    }

    function _renderTTTBoard(session) {
        var board = session.state || '_________';
        var status = session.status;
        var isMyTurn = (status === 'active') && _isMe(session, session.turn);
        var winCells = _findWinCells(board);

        var myMarker = session.my_marker || '';
        var iAmX, xPlayer, oPlayer;
        if (myMarker === 'X') {
            iAmX = true;
        } else if (myMarker === 'O') {
            iAmX = false;
        } else {
            iAmX = _isMe(session, session.challenger) || _isMe(session, session.first_turn);
        }

        if (iAmX) {
            xPlayer = 'You (X)';
            oPlayer = ratspeakDisplayNameHtml(_contactName(session.contact_hash), session.contact_hash) + ' (O)';
        } else {
            xPlayer = ratspeakDisplayNameHtml(_contactName(session.contact_hash), session.contact_hash) + ' (X)';
            oPlayer = 'You (O)';
        }

        var xTurnActive = (status === 'active') && session.turn && _isMe(session, session.first_turn) === _isMe(session, session.turn);
        var oTurnActive = (status === 'active') && session.turn && !xTurnActive;

        var html = '<div class="ttt-board-wrap">';

        html += '<div class="ttt-player-label' + (xTurnActive ? ' active-turn' : '') + '">' + xPlayer + '</div>';

        var markerClass = isMyTurn ? (iAmX ? ' my-marker-x' : ' my-marker-o') : '';
        html += '<div class="ttt-grid' + (isMyTurn ? ' your-turn' : '') + markerClass + '" role="grid" aria-label="Tic-Tac-Toe board">';
        for (var i = 0; i < 9; i++) {
            var cell = board[i];
            var classes = 'ttt-cell';
            if (cell === 'X') classes += ' marker-x';
            else if (cell === 'O') classes += ' marker-o';
            else if (isMyTurn && cell === '_') classes += ' clickable';

            if (winCells && winCells.indexOf(i) !== -1) classes += ' win-cell';

            var display = '';
            if (cell === 'X') {
                display = '<svg class="ttt-marker-svg" viewBox="0 0 50 50">' +
                    '<line x1="12" y1="12" x2="38" y2="38" stroke="currentColor" stroke-width="5" stroke-linecap="round"/>' +
                    '<line x1="38" y1="12" x2="12" y2="38" stroke="currentColor" stroke-width="5" stroke-linecap="round"/></svg>';
            } else if (cell === 'O') {
                display = '<svg class="ttt-marker-svg" viewBox="0 0 50 50">' +
                    '<circle cx="25" cy="25" r="15" stroke="currentColor" stroke-width="5" fill="none"/></svg>';
            }
            var cellLabel = 'Square ' + (i + 1) + ': ' + (cell === '_' ? 'empty' : cell);
            var canPlayCell = isMyTurn && cell === '_';
            html += '<button type="button" role="gridcell" class="' + classes + '" data-cell-index="' + i + '" aria-label="' + cellLabel + '" aria-disabled="' + (canPlayCell ? 'false' : 'true') + '"' + (canPlayCell ? '' : ' tabindex="-1"') + '>' + display + '</button>';
        }
        html += '</div>';

        html += '<div class="ttt-player-label' + (oTurnActive ? ' active-turn' : '') + '">' + oPlayer + '</div>';

        if (winCells && status === 'completed') {
            html += _renderWinLine(winCells);
        }

        if (status === 'pending') {
            var isPendingReceived = !_isMe(session, session.challenger);
            html += '<div class="ttt-board-overlay">' +
                (isPendingReceived ? 'Challenge received!' : 'Waiting for response...') +
            '</div>';
        }

        if (status === 'completed') {
            var overlayClass = 'ttt-game-over-overlay';
            var resultText = '';
            if (session.terminal === 'draw') {
                overlayClass += ' draw';
                resultText = 'Draw';
            } else if (_isMe(session, session.winner)) {
                overlayClass += ' won';
                resultText = 'You Won!';
            } else {
                overlayClass += ' lost';
                resultText = 'You Lost!';
            }
            html += '<div class="' + overlayClass + '">' +
                '<div class="ttt-game-over-text">' + resultText + '</div>' +
            '</div>';
        }

        html += '</div>';
        return html;
    }

    function _renderWinLine(cells) {
        var coords = cells.map(function(c) {
            return { x: (c % 3) * 33.33 + 16.67, y: Math.floor(c / 3) * 33.33 + 16.67 };
        });
        return '<svg class="ttt-win-line" viewBox="0 0 100 100" preserveAspectRatio="none">' +
            '<line x1="' + coords[0].x + '%" y1="' + coords[0].y + '%" ' +
            'x2="' + coords[2].x + '%" y2="' + coords[2].y + '%" ' +
            'stroke="var(--accent)" stroke-width="3" stroke-linecap="round" opacity="0.7">' +
            '<animate attributeName="stroke-dashoffset" from="200" to="0" dur="0.5s" fill="freeze"/>' +
            '</line></svg>';
    }

    function _findWinCells(board) {
        if (!board || board.length < 9) return null;
        for (var i = 0; i < WIN_LINES.length; i++) {
            var a = WIN_LINES[i][0], b = WIN_LINES[i][1], c = WIN_LINES[i][2];
            if (board[a] !== '_' && board[a] === board[b] && board[b] === board[c]) {
                return WIN_LINES[i];
            }
        }
        return null;
    }

    function _tttActiveStatusText(session) {
        var myMarker = session.my_marker
            || (_isMe(session, session.first_turn) ? 'X' : 'O');
        var theirMarker = myMarker === 'X' ? 'O' : 'X';
        if (_isMe(session, session.turn)) return 'Your turn (' + myMarker + ')';
        if (session.turn) {
            return (_contactName(session.contact_hash) || 'Opponent') +
                '\u2019s turn (' + theirMarker + ')';
        }
        return '';
    }

    function _tttDetailChips(session) {
        var marker = session.my_marker || (_isMe(session, session.first_turn) ? 'X' : 'O');
        return marker ? ['You are ' + marker] : [];
    }

    function _tttSessionDelta(record, previous) {
        var previousBoard = previous ? previous.state : null;
        if (record.game_id !== _selectedSessionId || !record.state ||
                !previousBoard || record.state === previousBoard) return;
        for (var cell = 0; cell < 9; cell++) {
            if ((previousBoard[cell] || '_') !== (record.state[cell] || '_')) {
                _animatingCell = cell;
                _animatingCellExpiry = Date.now() + 600;
                break;
            }
        }
    }

    function _handleTTTMove(session, cellIndex) {
        var board = (session.state || '_________').split('');
        if (board[cellIndex] !== '_') return;
        if (!_beginSessionAction(session.game_id)) return;

        var myMarker = session.my_marker || (_isMe(session, session.first_turn) ? 'X' : 'O');

        board[cellIndex] = myMarker;
        var newBoard = board.join('');
        var moveCount = (parseInt(session.move_count, 10) || 0) + 1;

        var winCells = _findWinCells(newBoard);
        var isDraw = !winCells && newBoard.indexOf('_') === -1;

        // Stash for rollback on game_action_result{ok:false}.
        _optimisticBackup[session.game_id] = {
            state: session.state,
            move_count: session.move_count,
            turn: session.turn,
            status: session.status,
            terminal: session.terminal,
            winner: session.winner,
            delivery_state: session.delivery_state,
        };

        // Mutate _allSessions live so the next render shows the optimistic move.
        session.state = newBoard;
        session.move_count = moveCount;
        session.delivery_state = 'pending';
        if (winCells) {
            session.terminal = 'win';
            session.winner = _getMyHash(session);
            session.status = 'completed';
            session.turn = '';
        } else if (isDraw) {
            session.terminal = 'draw';
            session.status = 'completed';
            session.turn = '';
        } else {
            session.turn = session.contact_hash;
        }

        _animatingCell = cellIndex;
        _animatingCellExpiry = Date.now() + 600;

        renderSessionList();
        renderDetail();

        if (winCells) {
            _maybeCelebrateWin(session);
        } else if (typeof haptic === 'function') {
            haptic('selection');
        }

        // Wire contract: backend expects "i" at top of payload, not nested under "move".
        // game_action_result handler rolls back on failure.
        RS.invoke('send_game_action', {
            args: {
                dest_hash: session.contact_hash,
                session_id: session.game_id,
                app_id: session.app_id || session.game || 'ttt',
                command: 'move',
                payload: { i: cellIndex },
            }
        }).then(function() {
            _finishSessionAction(session.game_id);
        }).catch(function() {
            _finishSessionAction(session.game_id);
            _handleGameActionFailure({
                session_id: session.game_id,
                command: 'move',
                reason: 'send_failed',
            });
        });
    }

    function _bindTTTCellEvents(session) {
        if (session.status !== 'active' || !_isMe(session, session.turn)) return;

        var cells = document.querySelectorAll('.ttt-cell.clickable');
        for (var i = 0; i < cells.length; i++) {
            cells[i].addEventListener('click', (function(idx) {
                return function() {
                    _handleTTTMove(session, idx);
                };
            })(parseInt(cells[i].getAttribute('data-cell-index'))));
        }
    }

    // Chess
    // Piece values for captured-tray sorting + material advantage display.
    var CHESS_PIECE_VALUES = { p: 1, n: 3, b: 3, r: 5, q: 9, k: 0 };
    var _chessSelected = {}; // { [session_id]: "e2" | null }

    function _chessActiveStatusText(session) {
        var myColor = session.my_color || (session.metadata && session.metadata.my_color) || '';
        var myMarker = myColor === 'b' ? 'Black' : 'White';
        var theirMarker = myColor === 'b' ? 'White' : 'Black';
        if (_isMe(session, session.turn)) return 'Your turn (' + myMarker + ')';
        if (session.turn) {
            return (_contactName(session.contact_hash) || 'Opponent') +
                '\u2019s turn (' + theirMarker + ')';
        }
        return '';
    }

    function _chessDetailChips(session) {
        var chips = [];
        var myColor = session.my_color || (session.metadata && session.metadata.my_color) || '';
        if (myColor === 'w') chips.push('White');
        if (myColor === 'b') chips.push('Black');
        if (session.in_check || (session.metadata && session.metadata.in_check)) chips.push('Check');
        var lastMove = session.last_move || (session.metadata && session.metadata.last_move) || '';
        if (lastMove) chips.push(lastMove.slice(0, 2) + '\u2192' + lastMove.slice(2, 4));
        return chips;
    }

    function _chessCelebrationOptions() {
        var styles = getComputedStyle(document.documentElement);
        return {
            count: 72,
            duration: 1900,
            colors: [
                (styles.getPropertyValue('--chess-light') || '#D4BC9E').trim(),
                (styles.getPropertyValue('--chess-dark') || '#9B8365').trim(),
                (styles.getPropertyValue('--accent') || '#D2693B').trim(),
                (styles.getPropertyValue('--status-online') || '#2E8B57').trim(),
                (styles.getPropertyValue('--ble-accent') || '#0E9AA7').trim(),
            ],
        };
    }

    function _chessActionPayload(action, session, payload) {
        // A FIDE claim reason makes the peer auto-accept instead of prompting.
        if (action === 'draw_offer' &&
                (session.draw_offer_reason === '3fr' || session.draw_offer_reason === '50m')) {
            return { r: session.draw_offer_reason };
        }
        return payload;
    }

    // FEN field 1 → { square: pieceCode } map. pieceCode is "w"|"b" + letter.
    function _chessFenToPieces(fen) {
        var pieces = {};
        if (!fen) return pieces;
        var boardField = fen.split(' ')[0] || '';
        var ranks = boardField.split('/');
        if (ranks.length !== 8) return pieces;
        var files = ['a','b','c','d','e','f','g','h'];
        for (var r = 0; r < 8; r++) {
            var rank = 8 - r;
            var s = ranks[r];
            var file = 0;
            for (var i = 0; i < s.length; i++) {
                var ch = s[i];
                if (ch >= '1' && ch <= '8') {
                    file += parseInt(ch, 10);
                } else {
                    var color = (ch === ch.toUpperCase()) ? 'w' : 'b';
                    pieces[files[file] + rank] = color + ch.toLowerCase();
                    file += 1;
                }
            }
        }
        return pieces;
    }

    // "white" keys hold BLACK's captures and vice versa (pieces shown next
    // to the capturing player are the ones they took).
    function _chessCaptured(pieces) {
        var starting = { w: { p: 8, n: 2, b: 2, r: 2, q: 1 }, b: { p: 8, n: 2, b: 2, r: 2, q: 1 } };
        var live = { w: { p: 0, n: 0, b: 0, r: 0, q: 0 }, b: { p: 0, n: 0, b: 0, r: 0, q: 0 } };
        Object.keys(pieces).forEach(function(sq) {
            var code = pieces[sq];
            var color = code[0];
            var kind = code[1];
            if (kind !== 'k' && live[color].hasOwnProperty(kind)) {
                live[color][kind] += 1;
            }
        });
        var diff = { w: {}, b: {} };
        ['p','n','b','r','q'].forEach(function(k) {
            diff.w[k] = Math.max(0, starting.w[k] - live.w[k]);
            diff.b[k] = Math.max(0, starting.b[k] - live.b[k]);
        });
        return { whiteCaptured: diff.b, blackCaptured: diff.w };
    }

    function _chessMaterialValue(captured) {
        return (captured.p || 0) * 1
             + (captured.n || 0) * 3
             + (captured.b || 0) * 3
             + (captured.r || 0) * 5
             + (captured.q || 0) * 9;
    }

    function _renderCapturedTray(captured, side) {
        // side 'w' shows the black pieces White has captured.
        var otherColor = (side === 'w') ? 'b' : 'w';
        var order = ['q','r','b','n','p']; // high-value first
        var html = '<div class="chess-captured-tray" data-side="' + side + '">';
        var any = false;
        for (var i = 0; i < order.length; i++) {
            var kind = order[i];
            var count = captured[kind] || 0;
            for (var j = 0; j < count; j++) {
                html += '<svg class="chess-captured-piece" viewBox="0 0 45 45" aria-hidden="true">' +
                    '<use href="/static/assets/chess-pieces.svg#' + otherColor + kind + '"/></svg>';
                any = true;
            }
        }
        html += '</div>';
        return any ? html : '<div class="chess-captured-tray" data-side="' + side + '"></div>';
    }

    function _renderChessBoard(session) {
        var fen = session.fen || (session.metadata && session.metadata.fen) || 'rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1';
        var pieces = _chessFenToPieces(fen);
        var myColor = session.my_color || ((session.metadata && session.metadata.my_color) || '');
        var status = session.status;
        var isMyTurn = (status === 'active') && _isMe(session, session.turn);
        var legalMoves = session.legal_moves || (session.metadata && session.metadata.legal_moves) || [];
        var lastMove = session.last_move || (session.metadata && session.metadata.last_move) || '';
        var inCheck = !!(session.in_check || (session.metadata && session.metadata.in_check));
        var selected = _chessSelected[session.game_id] || null;

        // White at bottom unless my_color=='b'; flip rank iteration accordingly.
        var orient = (myColor === 'b') ? 'b' : 'w';

        // Legal dests for the selected square (5-char UCI for promotion).
        var legalDestsFromSel = {};
        if (selected) {
            for (var i = 0; i < legalMoves.length; i++) {
                var m = legalMoves[i];
                if (m.slice(0, 2) === selected) {
                    legalDestsFromSel[m.slice(2, 4)] = true;
                }
            }
        }

        // Tray belongs to the label's side (Black label = pieces Black captured).
        var captured = _chessCaptured(pieces);
        var whiteMaterial = _chessMaterialValue(captured.whiteCaptured);
        var blackMaterial = _chessMaterialValue(captured.blackCaptured);
        var whiteAdvantage = whiteMaterial - blackMaterial;

        var opponentHash = session.contact_hash;
        var opponentName = ratspeakDisplayNameHtml(_contactName(opponentHash), opponentHash);
        var opponentColor = (myColor === 'w') ? 'b' : (myColor === 'b' ? 'w' : '');
        var myColorLabel = myColor === 'w' ? 'White' : (myColor === 'b' ? 'Black' : '');
        var opponentColorLabel = opponentColor === 'w' ? 'White' : (opponentColor === 'b' ? 'Black' : '');

        var myTurnActive = (status === 'active') && _isMe(session, session.turn);
        var opponentTurnActive = (status === 'active') && !myTurnActive && session.turn;

        function advPill(side) {
            var mat = (side === 'w') ? whiteAdvantage : -whiteAdvantage;
            if (mat > 0) return ' <span class="chess-material-pill">+' + mat + '</span>';
            return '';
        }

        // Top→bottom: opponent label+tray, board, my label+tray.
        var oppSide = (orient === 'w') ? 'b' : 'w'; // opponent's color
        var mySide = orient;

        var html = '<div class="chess-board-wrap' + (myTurnActive ? ' your-turn' : '') + '" data-orient="' + orient + '">';

        html += '<div class="chess-player-row' + (opponentTurnActive ? ' active-turn' : '') + '">' +
            '<span class="chess-player-name">' + opponentName +
                (opponentColorLabel ? ' (' + opponentColorLabel + ')' : '') +
                advPill(oppSide) +
            '</span>' +
        '</div>';
        html += _renderCapturedTray(oppSide === 'w' ? captured.whiteCaptured : captured.blackCaptured, oppSide);

        html += '<div class="chess-board" role="grid">';
        var files = ['a','b','c','d','e','f','g','h'];
        for (var rIdx = 0; rIdx < 8; rIdx++) {
            var rank = (orient === 'w') ? (8 - rIdx) : (rIdx + 1);
            for (var fIdx = 0; fIdx < 8; fIdx++) {
                var file = (orient === 'w') ? files[fIdx] : files[7 - fIdx];
                var sq = file + rank;
                // a1 is dark (file+rank odd → light, 0-indexed).
                var fileNum = files.indexOf(file);
                var isLight = ((fileNum + rank) % 2 === 1);
                var classes = ['chess-square', isLight ? 'light' : 'dark'];
                var piece = pieces[sq] || null;
                var isSelected = selected === sq;
                var isLegalTarget = legalDestsFromSel[sq];
                var isLastFrom = lastMove && lastMove.slice(0, 2) === sq;
                var isLastTo = lastMove && lastMove.slice(2, 4) === sq;

                if (isSelected) classes.push('selected');
                if (isLegalTarget) classes.push(piece ? 'legal-target has-piece' : 'legal-target');
                if (isLastFrom) classes.push('last-move-from');
                if (isLastTo) classes.push('last-move-to');

                // in_check is relative to side-to-move; glow that side's king.
                if (inCheck && piece && piece[1] === 'k') {
                    var sideToMove = (fen.split(' ')[1] || 'w');
                    if (piece[0] === sideToMove) classes.push('in-check');
                }

                var pieceHtml = '';
                if (piece) {
                    pieceHtml = '<svg class="chess-piece" viewBox="0 0 45 45" aria-hidden="true">' +
                        '<use href="/static/assets/chess-pieces.svg#' + piece + '"/></svg>';
                }

                // Inline rank/file labels so the board reads without a coords strip.
                var coordHtml = '';
                if (fIdx === 0) coordHtml += '<span class="chess-coord chess-coord-rank">' + rank + '</span>';
                if (rIdx === 7) coordHtml += '<span class="chess-coord chess-coord-file">' + file + '</span>';

                var clickable = isMyTurn && (isSelected || isLegalTarget || (piece && piece[0] === myColor));
                if (clickable) classes.push('clickable');

                var pieceName = piece ? _chessPieceName(piece) : 'empty';
                html += '<button type="button" role="gridcell" class="' + classes.join(' ') + '" data-square="' + sq + '" aria-label="' + sq + ': ' + pieceName + '" aria-disabled="' + (clickable ? 'false' : 'true') + '"' + (clickable ? '' : ' tabindex="-1"') + '>' + coordHtml + pieceHtml + '</button>';
            }
        }
        html += '</div>';

        html += _renderCapturedTray(mySide === 'w' ? captured.whiteCaptured : captured.blackCaptured, mySide);
        html += '<div class="chess-player-row' + (myTurnActive ? ' active-turn' : '') + '">' +
            '<span class="chess-player-name">You' +
                (myColorLabel ? ' (' + myColorLabel + ')' : '') +
                advPill(mySide) +
            '</span>' +
        '</div>';

        if (status === 'pending') {
            var isPendingReceived = !_isMe(session, session.challenger);
            html += '<div class="chess-board-overlay">' +
                (isPendingReceived ? 'Challenge received!' : 'Waiting for response...') +
            '</div>';
        }

        if (status === 'completed') {
            var overlayClass = 'game-over-overlay';
            var resultText = '';
            var reasonText = _chessTerminalReasonText(session.terminal_reason);
            if (session.terminal === 'draw') {
                overlayClass += ' draw';
                resultText = 'Draw';
            } else if (_isMe(session, session.winner)) {
                overlayClass += ' won';
                resultText = 'You Won!';
            } else {
                overlayClass += ' lost';
                resultText = 'You Lost!';
            }
            html += '<div class="' + overlayClass + '">' +
                '<div class="chess-game-over-text">' + resultText + '</div>' +
                (reasonText ? '<div class="chess-game-over-reason">' + reasonText + '</div>' : '') +
            '</div>';
        }

        html += '</div>';
        return html;
    }

    function _chessTerminalReasonText(code) {
        switch (code) {
            case 'cm':  return 'Checkmate';
            case 'sm':  return 'Stalemate';
            case 'ins': return 'Insufficient material';
            case '3fr': return 'Threefold repetition';
            case '50m': return 'Fifty-move rule';
            case 'rsn': return 'Resignation';
            case 'agr': return 'By agreement';
            default:    return '';
        }
    }

    function _chessPieceName(piece) {
        if (!piece || piece.length < 2) return 'empty';
        var color = piece[0] === 'w' ? 'white' : 'black';
        var names = { p: 'pawn', n: 'knight', b: 'bishop', r: 'rook', q: 'queen', k: 'king' };
        return color + ' ' + (names[piece[1]] || 'piece');
    }

    function _bindChessSquareEvents(session) {
        if ((session.app_id || session.game) !== 'chess') return;
        if (session.status !== 'active' || !_isMe(session, session.turn)) return;

        var squares = document.querySelectorAll('.chess-square.clickable');
        for (var i = 0; i < squares.length; i++) {
            squares[i].addEventListener('click', (function(sq) {
                return function() { _handleChessSquareClick(session, sq); };
            })(squares[i].getAttribute('data-square')));
        }
    }

    function _handleChessSquareClick(session, sq) {
        var sid = session.game_id;
        var myColor = session.my_color || '';
        var legalMoves = session.legal_moves || [];
        var fen = session.fen || '';
        var pieces = _chessFenToPieces(fen);
        var selected = _chessSelected[sid] || null;
        var pieceHere = pieces[sq] || null;

        if (!selected) {
            if (pieceHere && pieceHere[0] === myColor) {
                _chessSelected[sid] = sq;
                renderDetail();
            }
            return;
        }

        if (selected === sq) {
            _chessSelected[sid] = null;
            renderDetail();
            return;
        }

        if (pieceHere && pieceHere[0] === myColor) {
            _chessSelected[sid] = sq;
            renderDetail();
            return;
        }

        var base = selected + sq;
        var nonPromoLegal = legalMoves.indexOf(base) !== -1;
        var promoLegal = ['q','r','b','n'].filter(function(p) {
            return legalMoves.indexOf(base + p) !== -1;
        });

        if (nonPromoLegal) {
            _chessSelected[sid] = null;
            _sendChessMove(session, base);
            return;
        }

        if (promoLegal.length > 0) {
            _showPromotionChooser(session, base, promoLegal, sq);
            return;
        }

        _chessSelected[sid] = null;
        renderDetail();
    }

    function _showPromotionChooser(session, baseUci, available, destSq) {
        var sid = session.game_id;
        var myColor = session.my_color || 'w';
        var existing = document.getElementById('chess-promotion-chooser');
        if (existing) existing.remove();

        var wrap = document.createElement('div');
        wrap.id = 'chess-promotion-chooser';
        wrap.className = 'chess-promotion-chooser';
        var order = ['q','r','b','n']; // standard order
        var chosenPieceHtml = order.filter(function(p) { return available.indexOf(p) !== -1; }).map(function(p) {
            return '<button class="chess-promotion-option" data-piece="' + p + '" aria-label="Promote to ' + p + '">' +
                '<svg viewBox="0 0 45 45"><use href="/static/assets/chess-pieces.svg#' + myColor + p + '"/></svg>' +
            '</button>';
        }).join('');
        wrap.innerHTML = chosenPieceHtml;

        var destEl = document.querySelector('.chess-square[data-square="' + destSq + '"]');
        var board = document.querySelector('.chess-board');
        if (destEl && board) {
            var br = board.getBoundingClientRect();
            var dr = destEl.getBoundingClientRect();
            wrap.style.left = (dr.left - br.left) + 'px';
            wrap.style.top = (dr.top - br.top) + 'px';
            wrap.style.width = dr.width + 'px';
            wrap.style.height = (dr.height * 4) + 'px';
            board.appendChild(wrap);
        } else {
            document.body.appendChild(wrap);
        }

        var finishPromotion = function(piece) {
            document.removeEventListener('click', dismiss, true);
            document.removeEventListener('keydown', escDismiss, true);
            wrap.remove();
            _chessSelected[sid] = null;
            if (piece) {
                _sendChessMove(session, baseUci + piece);
            } else {
                renderDetail();
            }
        };
        wrap.querySelectorAll('.chess-promotion-option').forEach(function(btn) {
            btn.addEventListener('click', function(e) {
                e.stopPropagation();
                finishPromotion(btn.getAttribute('data-piece'));
            });
        });

        var dismiss = function(e) {
            if (wrap.contains(e.target)) return;
            finishPromotion(null);
        };
        var escDismiss = function(e) {
            if (e.key === 'Escape') {
                e.preventDefault();
                finishPromotion(null);
            } else if (e.key === 'Enter') {
                e.preventDefault();
                finishPromotion(available.indexOf('q') !== -1 ? 'q' : available[0]);
            }
        };
        // Defer a tick so the click doesn't immediately dismiss.
        setTimeout(function() {
            document.addEventListener('click', dismiss, true);
            document.addEventListener('keydown', escDismiss, true);
        }, 0);
    }

    function _sendChessMove(session, uci) {
        var sid = session.game_id;
        var fen = session.fen || '';
        var from = uci.slice(0, 2);
        var to = uci.slice(2, 4);
        var pieces = _chessFenToPieces(fen);
        var moved = pieces[from];
        if (!moved) return; // shouldn't happen — we validated via legal_moves
        if (!_beginSessionAction(sid)) return;

        _optimisticBackup[sid] = {
            fen: session.fen,
            state: session.state, // may be undefined — harmless
            move_count: session.move_count,
            turn: session.turn,
            status: session.status,
            terminal: session.terminal,
            winner: session.winner,
            delivery_state: session.delivery_state,
            legal_moves: session.legal_moves,
            last_move: session.last_move,
            in_check: session.in_check,
            draw_offer_reason: session.draw_offer_reason,
            terminal_reason: session.terminal_reason,
        };

        // Optimistic FEN update; authoritative server FEN overwrites in a beat.
        var promoPiece = (uci.length === 5) ? uci[4] : null;
        delete pieces[from];
        if (moved[1] === 'p' && !pieces[to] && from[0] !== to[0]) {
            var epCapSq = to[0] + from[1];
            delete pieces[epCapSq];
        }
        if (moved[1] === 'k' && Math.abs(from.charCodeAt(0) - to.charCodeAt(0)) === 2) {
            var rank = from[1];
            var rookFromFile = (to[0] === 'g') ? 'h' : 'a';
            var rookToFile   = (to[0] === 'g') ? 'f' : 'd';
            var rookKey = rookFromFile + rank;
            if (pieces[rookKey]) {
                var rookPiece = pieces[rookKey];
                delete pieces[rookKey];
                pieces[rookToFile + rank] = rookPiece;
            }
        }
        pieces[to] = promoPiece ? (moved[0] + promoPiece) : moved;

        session.fen = _chessPiecesToFen(pieces, session.fen);
        session.last_move = uci;
        session.move_count = (parseInt(session.move_count, 10) || 0) + 1;
        session.turn = session.contact_hash; // will pass back if move is rejected
        session.legal_moves = []; // clear until server re-sends
        session.in_check = false;
        session.delivery_state = 'pending';

        renderSessionList();
        renderDetail();

        if (typeof haptic === 'function') haptic('selection');

        RS.invoke('send_game_action', {
            args: {
                dest_hash: session.contact_hash,
                session_id: sid,
                app_id: 'chess',
                command: 'move',
                payload: { m: uci },
            }
        }).then(function() {
            _finishSessionAction(sid);
        }).catch(function() {
            _finishSessionAction(sid);
            _handleGameActionFailure({
                session_id: sid,
                command: 'move',
                reason: 'send_failed',
            });
        });
    }

    // Approximations OK — authoritative server FEN overwrites in a beat.
    function _chessPiecesToFen(pieces, refFen) {
        var files = ['a','b','c','d','e','f','g','h'];
        var rows = [];
        for (var rank = 8; rank >= 1; rank--) {
            var row = '';
            var empty = 0;
            for (var f = 0; f < 8; f++) {
                var sq = files[f] + rank;
                var p = pieces[sq];
                if (!p) { empty += 1; continue; }
                if (empty > 0) { row += empty; empty = 0; }
                var letter = p[1];
                row += (p[0] === 'w') ? letter.toUpperCase() : letter;
            }
            if (empty > 0) row += empty;
            rows.push(row);
        }
        var board = rows.join('/');
        var tail = ' w KQkq - 0 1';
        if (refFen) {
            var parts = refFen.split(' ');
            if (parts.length >= 6) {
                var side = (parts[1] === 'w') ? 'b' : 'w';
                tail = ' ' + side + ' ' + parts[2] + ' ' + parts[3] + ' ' + parts[4] + ' ' + parts[5];
            }
        }
        return board + tail;
    }

    function _gameActions(session) {
        var appId = _appId(session);
        var manifest = _manifestsById[appId];
        if (manifest && Array.isArray(manifest.actions)) return manifest.actions;
        var view = _gameView(appId);
        return view && Array.isArray(view.actions) ? view.actions : [];
    }

    function _gameSupportsAction(session, action) {
        return _gameActions(session).indexOf(action) !== -1;
    }

    function _renderStandardActiveControls(session, drawOfferLabel) {
        var html = '';
        if (_sessionValue(session, 'draw_offered', false) &&
                _gameSupportsAction(session, 'draw_accept') &&
                _gameSupportsAction(session, 'draw_decline')) {
            var drawOwner = _drawOfferOwner(session);
            if (drawOwner && !_isMe(session, drawOwner)) {
                html += '<button class="nr-btn games-ctrl-accept" id="games-draw-accept-btn">Accept Draw</button>';
                html += '<button class="nr-btn nr-btn-secondary" id="games-draw-decline-btn">Decline Draw</button>';
            } else {
                html += '<span class="games-ctrl-waiting">Waiting for opponent to respond...</span>';
            }
            html += '<span class="games-ctrl-separator"></span>';
        } else if (_gameSupportsAction(session, 'draw_offer')) {
            html += '<button class="nr-btn nr-btn-secondary" id="games-draw-offer-btn">' +
                escapeHtml(drawOfferLabel || 'Offer Draw') + '</button>';
        }
        if (_gameSupportsAction(session, 'resign')) {
            html += '<button class="nr-btn nr-btn-danger" id="games-resign-btn">Resign</button>';
        }
        return html;
    }

    function _chessRenderActiveControls(session) {
        var drawLabel = '';
        if (session.draw_offer_reason === '3fr') drawLabel = 'Claim threefold';
        if (session.draw_offer_reason === '50m') drawLabel = 'Claim 50-move';
        return _renderStandardActiveControls(session, drawLabel);
    }

    function _renderControls(session) {
        var status = session.status;
        var html = '';

        if (status === 'pending') {
            if (!_isMe(session, session.challenger)) {
                html += '<button class="nr-btn games-ctrl-accept" id="games-accept-btn">Accept</button>';
                html += '<button class="nr-btn nr-btn-danger" id="games-decline-btn">Decline</button>';
            } else {
                html += '<span class="games-ctrl-waiting">Waiting for opponent to respond...</span>';
                html += '<button class="nr-btn nr-btn-secondary" id="games-cancel-btn">Cancel</button>';
            }
        } else if (status === 'active') {
            var view = _gameView(_appId(session));
            html += view && view.renderActiveControls
                ? view.renderActiveControls(session)
                : _renderStandardActiveControls(session, '');
        } else if (status === 'completed' || status === 'declined' || status === 'expired') {
            html += '<button class="nr-btn" id="games-rematch-btn">Rematch</button>';
        }

        return html;
    }

    function _bindControlEvents(session) {
        _bindBtn('games-accept-btn', function() {
            var btn = document.getElementById('games-accept-btn');
            if (btn && btn.disabled) return;
            if (btn) { btn.disabled = true; btn.textContent = 'Accepting…'; }
            _sendAction(session, 'accept');
        });
        _bindBtn('games-decline-btn', function() {
            var btn = document.getElementById('games-decline-btn');
            if (btn && btn.disabled) return;
            if (btn) { btn.disabled = true; btn.textContent = 'Declining…'; }
            _sendAction(session, 'decline');
        });
        _bindBtn('games-cancel-btn', function() {
            var btn = document.getElementById('games-cancel-btn');
            var doCancel = function() {
                if (btn) { btn.disabled = true; btn.textContent = 'Cancelling…'; }
                _sendAction(session, 'decline');
            };
            if (typeof rsConfirm === 'function') {
                rsConfirm({
                    message: 'Cancel this challenge? Your opponent will be notified.',
                    title: 'Cancel challenge',
                    confirmText: 'Cancel challenge',
                    danger: true,
                }).then(function(ok) { if (ok) doCancel(); });
            } else if (typeof showToast === 'function') {
                showToast('Confirmation dialog unavailable', 'toast-red', 3000);
            }
        });
        _bindBtn('games-resign-btn', function() {
            if (typeof rsConfirm === 'function') {
                rsConfirm({
                    message: 'Are you sure you want to resign?',
                    title: 'Resign',
                    confirmText: 'Resign',
                    danger: true,
                }).then(function(ok) {
                    if (ok) _sendAction(session, 'resign');
                });
            } else if (typeof showToast === 'function') {
                showToast('Confirmation dialog unavailable', 'toast-red', 3000);
            }
        });
        _bindBtn('games-rematch-btn', function() {
            startNewGame(session.app_id || session.game || 'ttt', session.contact_hash);
        });
        _bindBtn('games-draw-offer-btn', function() {
            var payload = {};
            var view = _gameView(_appId(session));
            if (view && view.actionPayload) {
                payload = view.actionPayload('draw_offer', session, payload) || payload;
            }
            _sendAction(session, 'draw_offer', payload);
        });
        _bindBtn('games-draw-accept-btn', function() {
            _sendAction(session, 'draw_accept');
        });
        _bindBtn('games-draw-decline-btn', function() {
            _sendAction(session, 'draw_decline');
        });
        var view = _gameView(_appId(session));
        if (view && view.bindControls) {
            view.bindControls(session, {
                bindButton: _bindBtn,
                sendAction: function(action, payload) {
                    _sendAction(session, action, payload);
                },
            });
        }
    }

    function _bindBtn(id, handler) {
        var el = document.getElementById(id);
        if (el) el.addEventListener('click', handler);
    }

    function _sendAction(session, action, payload) {
        var sessionId = session.game_id;
        if (!_beginSessionAction(sessionId)) return;
        RS.invoke('send_game_action', {
            args: {
                dest_hash: session.contact_hash,
                session_id: session.game_id,
                app_id: session.app_id || session.game || 'ttt',
                command: action,
                payload: payload || {},
            }
        }).then(function() {
            _finishSessionAction(sessionId);
            // Backend rejections are emitted through game_action_result so
            // optimistic rollback and user feedback have one ordered path.
            // The promise catch below remains for IPC failures that cannot
            // produce a backend event.
        }).catch(function() {
            _finishSessionAction(sessionId);
            if (typeof showToast === 'function') {
                showToast(_reasonToMessage('send_failed', action), 'toast-red', 4000);
            }
        });
    }

    // View adapters can request immediate, presentation-only move feedback
    // without owning transport or protocol authority. The runtime remains the
    // source of truth and its next session snapshot replaces this local state.
    function _sendGameViewMove(session, payload, optimistic) {
        var sessionId = session && session.game_id;
        if (!sessionId || !_beginSessionAction(sessionId)) return false;

        optimistic = optimistic || {};
        var fields = Array.isArray(optimistic.fields) ? optimistic.fields : [];
        var adapterFields = RS.games.optimistic.captureFields(session, fields);
        _optimisticBackup[sessionId] = {
            state: session.state,
            move_count: session.move_count,
            turn: session.turn,
            status: session.status,
            terminal: session.terminal,
            winner: session.winner,
            delivery_state: session.delivery_state,
            adapter_fields: adapterFields,
        };

        if (typeof optimistic.apply === 'function') optimistic.apply(session);
        session.delivery_state = 'pending';
        renderSessionList();
        renderDetail();
        if (typeof haptic === 'function') haptic('selection');

        RS.invoke('send_game_action', {
            args: {
                dest_hash: session.contact_hash,
                session_id: session.game_id,
                app_id: session.app_id || session.game,
                command: 'move',
                payload: payload || {},
            }
        }).then(function() {
            _finishSessionAction(sessionId);
        }).catch(function() {
            _finishSessionAction(sessionId);
            _handleGameActionFailure({
                session_id: sessionId,
                command: 'move',
                reason: 'send_failed',
            });
        });
        return true;
    }

    function _reasonToMessage(reason, command) {
        switch (reason) {
            case 'invalid_params':       return 'Bad action parameters';
            case 'session_terminal':     return 'Session already ended';
            case 'session_exists':       return 'That game session already exists';
            case 'session_not_found':    return 'This game session is no longer available';
            case 'invalid_state':        return 'That action is not available right now';
            case 'dispatch_failed':      return 'Action rejected by game rules';
            case 'not_your_turn':        return 'Not your turn';
            case 'unauthorized_sender':  return 'This action is not from the game opponent';
            case 'session_expired':      return 'This game has expired';
            case 'unsupported_app':      return 'This game version is not supported';
            case 'protocol_error':       return 'Invalid game action';
            case 'storage_failed':       return 'Game state could not be saved';
            case 'resend_required':       return 'Action saved locally — tap Resend to retry';
            case 'lxmf_not_initialized': return 'Messaging not ready — wait a moment';
            case 'pack_failed':          return 'Action rejected — invalid envelope';
            case 'send_failed':
            default:
                return command === 'move'
                    ? 'Move couldn’t be delivered — tap Resend'
                    : 'Action couldn’t be delivered';
        }
    }

    // Tauri command rejections that happen before LRGP dispatch do not have a
    // backend game_action_result event to restore an optimistic board. Keep
    // one rollback path for both IPC rejection and emitted protocol results.
    function _handleGameActionFailure(data) {
        if (!data || !data.session_id) return;
        var sid = data.session_id;
        var reason = data.reason || 'send_failed';

        // A failed durable-outbox rollback intentionally leaves the canonical
        // board advanced with its exact envelope available to Resend.
        if (reason === 'resend_required') {
            delete _optimisticBackup[sid];
            for (var pendingIndex = 0; pendingIndex < _allSessions.length; pendingIndex++) {
                if (_allSessions[pendingIndex].game_id === sid) {
                    _allSessions[pendingIndex].delivery_state = 'failed';
                    break;
                }
            }
            renderSessionList();
            if (sid === _selectedSessionId) renderDetail();
            if (typeof showToast === 'function') {
                showToast(_reasonToMessage(reason, data.command), 'toast-red', 5000);
            }
            return;
        }

        var backup = _optimisticBackup[sid];
        if (backup) {
            for (var i = 0; i < _allSessions.length; i++) {
                if (_allSessions[i].game_id !== sid) continue;
                var session = _allSessions[i];
                session.state = backup.state;
                session.move_count = backup.move_count;
                session.turn = backup.turn;
                session.status = backup.status;
                session.terminal = backup.terminal;
                session.winner = backup.winner;
                session.delivery_state = backup.delivery_state;
                if (backup.fen !== undefined) session.fen = backup.fen;
                if (backup.legal_moves !== undefined) session.legal_moves = backup.legal_moves;
                if (backup.last_move !== undefined) session.last_move = backup.last_move;
                if (backup.in_check !== undefined) session.in_check = backup.in_check;
                if (backup.draw_offer_reason !== undefined) session.draw_offer_reason = backup.draw_offer_reason;
                if (backup.terminal_reason !== undefined) session.terminal_reason = backup.terminal_reason;
                if (backup.adapter_fields) {
                    RS.games.optimistic.restoreFields(session, backup.adapter_fields);
                }
                break;
            }
            delete _optimisticBackup[sid];
            renderSessionList();
            if (sid === _selectedSessionId) renderDetail();
        }

        if (typeof showToast === 'function') {
            showToast(_reasonToMessage(reason, data.command), 'toast-red', 4000);
        }
        if (typeof haptic === 'function') haptic('error');
    }

    function showNewGameDialog() {
        _showNewGameSheet();
    }

    function _showNewGameSheet() {
        if (typeof haptic === 'function') haptic('selection');

        var existing = document.getElementById('games-new-sheet-overlay');
        if (existing) existing.remove();
        existing = document.getElementById('games-new-sheet');
        if (existing) existing.remove();

        var contacts = _getContacts();
        var sorted = contacts.slice().sort(function(a, b) {
            return (a.display_name || '').localeCompare(b.display_name || '');
        });

        var contactsHtml = '';
        if (sorted.length === 0) {
            contactsHtml = '<div class="games-sheet-empty">' +
                '<span class="games-sheet-empty-icon" aria-hidden="true">' +
                    '<svg viewBox="0 0 24 24"><circle cx="9" cy="8" r="3"></circle><path d="M3.5 18c.5-3 2.4-4.7 5.5-4.7 1.5 0 2.7.4 3.6 1.1"></path><path d="M17 11v7M13.5 14.5h7"></path></svg>' +
                '</span>' +
                '<span class="games-sheet-empty-copy">' +
                    '<span class="games-sheet-empty-title">No contacts yet</span>' +
                    '<span class="games-sheet-empty-hint">Add someone before starting a game.</span>' +
                '</span>' +
                '<button type="button" class="nr-btn nr-btn-secondary games-sheet-open-contacts" id="games-sheet-open-contacts">Open Contacts</button>' +
            '</div>';
        } else {
            for (var i = 0; i < sorted.length; i++) {
                var c = sorted[i];
                var name = c.display_name || 'Anonymous';
                var avatar = (typeof identityAvatar === 'function') ? identityAvatar(c.hash, 32) : '';
                contactsHtml += '<button type="button" class="games-sheet-contact-row" data-hash="' + escapeHtml(c.hash) + '" aria-pressed="false">' +
                    '<span class="games-sheet-contact-avatar">' + avatar + '</span>' +
                    '<span class="games-sheet-contact-copy">' +
                        '<span class="games-sheet-contact-name">' + ratspeakDisplayNameHtml(name, c) + '</span>' +
                        '<span class="games-sheet-contact-hash">' + escapeHtml(shortHash(c.hash, 8, 4)) + '</span>' +
                    '</span>' +
                '</button>';
            }
        }

        var manifests = RS.games.views.supportedManifests(Object.keys(_manifestsById).map(function(appId) {
            return _manifestsById[appId];
        }));
        if (manifests.length === 0) {
            manifests = [
                { app_id: 'ttt', display_name: 'Tic-Tac-Toe', icon: 'ttt', session_type: 'turn_based' },
                { app_id: 'chess', display_name: 'Chess', icon: 'chess', session_type: 'turn_based' },
                { app_id: 'four_in_a_row', display_name: 'Four in a Row', icon: 'four_in_a_row', session_type: 'turn_based' },
            ];
        }
        manifests.sort(function(a, b) {
            return (a.display_name || a.app_id).localeCompare(b.display_name || b.app_id);
        });
        var gameCardsHtml = manifests.map(function(manifest, index) {
            var appId = manifest.app_id || '';
            var name = manifest.display_name || appId;
            return '<button type="button" class="games-sheet-game-card' + (index === 0 ? ' selected' : '') + '" data-app-id="' + escapeHtml(appId) + '" aria-pressed="' + (index === 0 ? 'true' : 'false') + '">' +
                '<span class="game-card-icon">' + _gameIconMarkup(appId) + '</span>' +
                '<span class="games-sheet-game-copy"><span class="games-sheet-game-name">' + escapeHtml(name) + '</span></span>' +
                '<span class="games-sheet-game-check" aria-hidden="true">✓</span>' +
            '</button>';
        }).join('');

        var shell = RS.sheetShell.create({ sheetClass: 'bottom-sheet games-new-dialog' });
        shell.overlay.id = 'games-new-sheet-overlay';
        shell.sheet.id = 'games-new-sheet';
        shell.sheet.setAttribute('role', 'dialog');
        shell.sheet.setAttribute('aria-modal', 'true');
        shell.sheet.setAttribute('aria-labelledby', 'games-new-sheet-title');
        shell.sheet._gamesPreviousFocus = document.activeElement;
        shell.sheet.innerHTML = '<div class="bottom-sheet-handle"></div>' +
            '<div class="bottom-sheet-header">' +
                '<div>' +
                    '<div class="bottom-sheet-title" id="games-new-sheet-title">New game</div>' +
                    '<div class="games-sheet-subtitle">Choose what to play and who to challenge.</div>' +
                '</div>' +
                '<button type="button" class="bottom-sheet-close" id="games-sheet-close" aria-label="Close">&times;</button>' +
            '</div>' +
            '<div class="bottom-sheet-body">' +
                '<div class="games-sheet-section">' +
                    '<div class="games-sheet-header" id="games-sheet-game-label">Game</div>' +
                    '<div class="games-sheet-game-grid" role="group" aria-labelledby="games-sheet-game-label">' + gameCardsHtml + '</div>' +
                '</div>' +
                '<div class="games-sheet-section">' +
                    '<div class="games-sheet-header" id="games-sheet-opponent-label">Opponent</div>' +
                    '<div class="games-sheet-contact-list' + (sorted.length === 0 ? ' is-empty' : '') + '" role="group" aria-labelledby="games-sheet-opponent-label">' + contactsHtml + '</div>' +
                '</div>' +
            '</div>' +
            '<div class="bottom-sheet-footer games-sheet-footer">' +
                '<button type="button" class="rs-dialog-cancel games-sheet-cancel-btn" id="games-sheet-cancel">Cancel</button>' +
                '<button type="button" class="rs-dialog-confirm games-sheet-send-btn" id="games-sheet-send" disabled>Send Challenge</button>' +
            '</div>';

        RS.sheetShell.present(shell);
        var overlay = shell.overlay;
        var sheet = shell.sheet;

        var selectedHash = null;
        var selectedAppId = manifests[0] ? manifests[0].app_id : 'ttt';

        if (sheet) {
            sheet._ratspeakDismiss = function() {
                _closeNewGameSheet();
                return true;
            };
            sheet.querySelectorAll('.games-sheet-game-card').forEach(function(card) {
                card.addEventListener('click', function() {
                    if (typeof haptic === 'function') haptic('selection');
                    sheet.querySelectorAll('.games-sheet-game-card').forEach(function(c) {
                        c.classList.remove('selected');
                        c.setAttribute('aria-pressed', 'false');
                    });
                    this.classList.add('selected');
                    this.setAttribute('aria-pressed', 'true');
                    selectedAppId = this.dataset.appId || 'ttt';
                });
            });

            sheet.querySelectorAll('.games-sheet-contact-row').forEach(function(row) {
                row.addEventListener('click', function() {
                    if (typeof haptic === 'function') haptic('selection');
                    sheet.querySelectorAll('.games-sheet-contact-row').forEach(function(r) {
                        r.classList.remove('selected');
                        r.setAttribute('aria-pressed', 'false');
                    });
                    this.classList.add('selected');
                    this.setAttribute('aria-pressed', 'true');
                    selectedHash = this.dataset.hash;
                    var sendBtn = document.getElementById('games-sheet-send');
                    if (sendBtn) sendBtn.disabled = false;
                });
            });

            sheet.addEventListener('keydown', function(e) {
                if (e.key === 'Escape') {
                    e.preventDefault();
                    _closeNewGameSheet();
                    return;
                }
                if (e.key !== 'Tab') return;
                var focusable = sheet.querySelectorAll('button:not([disabled])');
                if (!focusable.length) return;
                var first = focusable[0];
                var last = focusable[focusable.length - 1];
                if (e.shiftKey && document.activeElement === first) {
                    e.preventDefault();
                    last.focus();
                } else if (!e.shiftKey && document.activeElement === last) {
                    e.preventDefault();
                    first.focus();
                }
            });

            requestAnimationFrame(function() {
                var selectedGame = sheet.querySelector('.games-sheet-game-card.selected');
                if (selectedGame) selectedGame.focus();
            });
        }

        _bindBtn('games-sheet-close', _closeNewGameSheet);
        _bindBtn('games-sheet-cancel', _closeNewGameSheet);
        _bindBtn('games-sheet-open-contacts', function() {
            _closeNewGameSheet(function() {
                if (typeof switchView === 'function') {
                    switchView('contacts', { pushState: true });
                }
            });
        });
        _bindBtn('games-sheet-send', function() {
            if (!selectedHash) return;
            if (typeof haptic === 'function') haptic('selection');
            _closeNewGameSheet();
            startNewGame(selectedAppId, selectedHash);
        });

        if (overlay) {
            overlay.addEventListener('click', function(e) {
                if (e.target === overlay) _closeNewGameSheet();
            });
        }

        if (typeof isMobile === 'function' && isMobile() && typeof initSheetSwipeDismiss === 'function') {
            initSheetSwipeDismiss('games-new-sheet', 'games-new-sheet-overlay', _closeNewGameSheet);
        }
    }

    function _closeNewGameSheet(done) {
        var sheet = document.getElementById('games-new-sheet');
        var previousFocus = sheet && sheet._gamesPreviousFocus;
        RS.sheetShell.dismiss({
            overlay: document.getElementById('games-new-sheet-overlay'),
            sheet: sheet,
        }, function() {
            if (typeof done === 'function') {
                done();
            } else if (previousFocus && previousFocus.focus) {
                previousFocus.focus();
            }
        });
    }

    function startNewGame(appId, contactHash) {
        var arr = new Uint8Array(8);
        crypto.getRandomValues(arr);
        var sessionId = '';
        for (var i = 0; i < arr.length; i++) {
            sessionId += ('0' + arr[i].toString(16)).slice(-2);
        }
        if (!_beginSessionAction(sessionId)) return;

        RS.invoke('send_game_action', {
            args: {
                dest_hash: contactHash,
                session_id: sessionId,
                app_id: appId,
                command: 'challenge',
                payload: {},
            }
        }).then(function(ack) {
            _finishSessionAction(sessionId);
            if (ack && ack.ok === false) {
                // game_action_result owns rejection feedback. Avoid showing
                // the same backend failure twice via both IPC completion and
                // the event stream.
                return;
            }
            if (typeof showToast === 'function') showToast('Challenge sent', 'toast-green', 2000);
            _selectedSessionId = (ack && ack.session_id) ? ack.session_id : sessionId;
            RS.invoke('get_all_game_sessions').then(function(sessions) {
                if (Array.isArray(sessions)) {
                    _allSessions = sessions;
                    renderSessionList();
                    renderDetail();
                }
            }).catch(function() {});
        }).catch(function() {
            _finishSessionAction(sessionId);
            if (typeof showToast === 'function') {
                showToast('Challenge failed', 'toast-red', 4000);
            }
        });
    }

    function _initGameEvents() {
        if (_gameEventsReady) return;
        if (typeof _startNetworkUnstableWatcher === 'function') {
            _startNetworkUnstableWatcher();
        }

        RS.listen('all_game_sessions', function(data) {
            var incoming = Array.isArray(data) ? data : [];
            var prevById = {};
            for (var i = 0; i < _allSessions.length; i++) {
                prevById[_allSessions[i].game_id] = _allSessions[i];
            }
            _allSessions = incoming;
            _markViewedSessionRead({ render: false });

            for (var j = 0; j < incoming.length; j++) {
                var record = incoming[j];
                var prev = prevById[record.game_id] || null;
                _handleSessionDelta(record, prev);
            }

            renderSessionList();
            if (_selectedSessionId) renderDetail();
            updateGamesBadge();
        });

        RS.listen('game_session_deleted', function(data) {
            if (data && data.session_id) _removeSessionLocal(data.session_id);
        });

        RS.listen('game_protocol_error', function(data) {
            if (!data) return;
            var message = data.message
                ? String(data.message).slice(0, 180)
                : _reasonToMessage(data.code || 'protocol_error', data.ref || 'action');
            if (typeof showToast === 'function') {
                showToast('Game action rejected: ' + message, 'toast-red', 5000);
            }
            if (typeof haptic === 'function') haptic('error');
        });

        // Success clears the optimistic backup; failure restores it.
        RS.listen('game_action_result', function(data) {
            if (!data || !data.session_id) return;
            var sid = data.session_id;

            if (data.ok === true) {
                delete _optimisticBackup[sid];
                return;
            }

            _handleGameActionFailure(data);
        });

        // Per-action signal from the runtime — forces a board redraw and badge
        // refresh even if the bulk `all_game_sessions` payload arrives stale or
        // the listener registration raced with Tauri global injection.
        RS.listen('game_action_received', function(data) {
            if (!data || !data.session_id) return;
            if (_isViewingSession(data.session_id)) {
                _markSessionReadLocal(data.session_id, { render: false, force: true });
            }
            if (data.session_id === _selectedSessionId) renderDetail();
            updateGamesBadge();
        });

        // Initial sync runs via gamesTabLoad() on first Games view activation.
        _gameEventsReady = true;
    }

    function _handleSessionDelta(record, prev) {
        if (!record || !record.game_id) return;

        var prevStatus = prev ? prev.status : null;

        var view = _gameView(_appId(record));
        if (view && view.onSessionDelta) {
            view.onSessionDelta(record, prev, _gameViewContext(record));
        }

        var isNew = !prev;
        if (isNew && typeof currentView !== 'undefined' && currentView !== 'games') {
            if (record.status === 'pending' && !_isMe(record, record.challenger)) {
                if (typeof showToast === 'function') showToast('\uD83C\uDFAE Game challenge from ' + _contactName(record.contact_hash), 'toast-green', 5000, function() { window.openGameSession(record.game_id); });
                if (typeof haptic === 'function') haptic('success');
                if (!window.__TAURI_INTERNALS__ && document.hidden && typeof rsNotify !== 'undefined') {
                    rsNotify.send({
                        title: 'Game challenge',
                        body: _contactName(record.contact_hash) + ' challenged you to a game'
                    });
                }
            }
        }

        // Toast on remote moves whenever the user isn't actively staring at
        // this game's board. `currentView !== 'games'` catches every other tab;
        // even on the games view a delta on a non-selected game still alerts.
        var movedSinceLast = prev &&
            _sessionValue(record, 'move_count', null) !==
            _sessionValue(prev, 'move_count', null);
        var notViewingThisGame = !_isViewingSession(record.game_id);
        if (movedSinceLast && notViewingThisGame && record.status === 'active') {
            if (typeof showToast === 'function') showToast('Game update from ' + _contactName(record.contact_hash), 'toast-blue', 3000, function() { window.openGameSession(record.game_id); });
            if (typeof haptic === 'function') haptic('light');
            if (!window.__TAURI_INTERNALS__ && document.hidden && typeof rsNotify !== 'undefined') {
                rsNotify.send({
                    title: 'Game update',
                    body: _contactName(record.contact_hash) + ' made a move'
                });
            }
        }

        // Eagerly nudge the badge on every unread delta so the dot appears even
        // mid-render of an unrelated view.
        var unreadChanged = !prev || (record.unread || 0) !== (prev.unread || 0);
        if (unreadChanged) updateGamesBadge();

        if (prev && record.status === 'completed' && prevStatus !== 'completed') _maybeCelebrateWin(record);
    }

    function updateGamesBadge() {
        var dot = document.getElementById('nav-games-unread');
        var bsDot = document.getElementById('bs-games-unread');
        var bbDot = document.getElementById('bb-more-unread');
        var total = 0;
        for (var i = 0; i < _allSessions.length; i++) {
            if (_allSessions[i].unread > 0 && !_isViewingSession(_allSessions[i].game_id)) total++;
        }
        if (dot) dot.style.display = (total > 0) ? '' : 'none';
        if (bsDot) bsDot.style.display = (total > 0) ? '' : 'none';
        if (bbDot) bbDot.style.display = (total > 0) ? '' : 'none';
    }

    function _initTabFilters() {
        var tabs = document.querySelectorAll('.games-tab');
        for (var i = 0; i < tabs.length; i++) {
            tabs[i].addEventListener('click', function() {
                var all = document.querySelectorAll('.games-tab');
                for (var j = 0; j < all.length; j++) all[j].classList.remove('active');
                this.classList.add('active');
                _activeFilter = this.getAttribute('data-filter');
                renderSessionList();
            });
        }
    }

    function _initNewGameBtn() {
        _bindBtn('games-new-btn', showNewGameDialog);
        RS.gestures.bindViewFabClick('games-fab-btn', showNewGameDialog);
    }

    window.gamesTabLoad = function() {
        _contactNameCache = {};
        _loadGameManifests().then(function() {
            renderSessionList();
            renderDetail();
        });
        RS.invoke('get_all_game_sessions').then(function(sessions) {
            if (Array.isArray(sessions)) {
                _allSessions = sessions;
                _markViewedSessionRead({ render: false });
                renderSessionList();
                updateGamesBadge();
            }
        }).catch(function() {});
    };

    // Deep-link entry point for notification taps (route lrgp:<session_id>).
    // Switches to the games view, refreshes sessions, then opens the board.
    window.openGameSession = function(sessionId) {
        if (!sessionId) return;
        if (typeof switchView === 'function') switchView('games');
        RS.invoke('get_all_game_sessions').then(function(sessions) {
            if (Array.isArray(sessions)) {
                _allSessions = sessions;
                renderSessionList();
            }
            selectSession(sessionId);
        }).catch(function() { selectSession(sessionId); });
    };

    window.updateGamesBadge = updateGamesBadge;

    window.gamesTabClear = function() {
        _allSessions = [];
        _selectedSessionId = null;
        _contactNameCache = {};
        _celebratedWins = {};
        renderSessionList();
        renderDetail();
        updateGamesBadge();
    };

    function _registerBuiltinGameViews() {
        if (!RS.games || !RS.games.views) {
            throw new Error('Game view registry must load before games_tab.js');
        }
        if (!RS.games.views.has('ttt')) {
            RS.games.views.register('ttt', {
                displayName: 'Tic-Tac-Toe',
                icon: '#',
                themeClass: 'games-theme-ttt',
                boardSelector: '.ttt-grid',
                actions: ['challenge', 'accept', 'decline', 'move', 'resign',
                    'draw_offer', 'draw_accept', 'draw_decline'],
                renderBoard: _renderTTTBoard,
                bindBoard: _bindTTTCellEvents,
                activeStatusText: _tttActiveStatusText,
                detailChips: _tttDetailChips,
                onSessionDelta: _tttSessionDelta,
                celebrationOptions: function() {
                    return { count: 48, duration: 1600 };
                },
            });
        }
        if (!RS.games.views.has('chess')) {
            RS.games.views.register('chess', {
                displayName: 'Chess',
                icon: '\u265E',
                themeClass: 'games-theme-chess',
                boardSelector: '.chess-board',
                actions: ['challenge', 'accept', 'decline', 'move', 'resign',
                    'draw_offer', 'draw_accept', 'draw_decline'],
                renderBoard: _renderChessBoard,
                bindBoard: _bindChessSquareEvents,
                activeStatusText: _chessActiveStatusText,
                detailChips: _chessDetailChips,
                renderActiveControls: _chessRenderActiveControls,
                celebrationOptions: _chessCelebrationOptions,
                actionPayload: _chessActionPayload,
            });
        }
    }

    function _init() {
        _loadGameManifests();
        _initTabFilters();
        _initNewGameBtn();
        _initGameEvents();
    }

    _registerBuiltinGameViews();

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', _init);
    } else {
        _init();
    }

})();
