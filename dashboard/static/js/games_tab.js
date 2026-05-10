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
                _contactNameCache[hash] = contacts[i].display_name || 'Anonymous';
                return _contactNameCache[hash];
            }
        }
        return _shortHash(hash, 8, 4);
    }

    function _shortHash(hash, front, back) {
        if (!hash) return '';
        if (typeof shortHash === 'function') return shortHash(hash, front || 8, back || 4);
        front = front || 8;
        back = back || 4;
        if (hash.length <= front + back + 1) return hash;
        return hash.substring(0, front) + '\u2026' + hash.slice(-back);
    }

    function _isMe(session, hash) {
        var myHash = _getMyHash(session);
        return myHash && hash === myHash;
    }

    function _appId(session) {
        return (session && (session.app_id || session.game)) || '';
    }

    function _celebrationOptions(session) {
        var appId = _appId(session);
        var opts = {
            count: appId === 'chess' ? 72 : 48,
            duration: appId === 'chess' ? 1900 : 1600,
        };

        if (appId === 'chess') {
            var cs = getComputedStyle(document.documentElement);
            opts.colors = [
                '#dce1e8',
                '#5f7185',
                (cs.getPropertyValue('--accent') || '#D2693B').trim(),
                (cs.getPropertyValue('--status-online') || '#2E8B57').trim(),
                (cs.getPropertyValue('--ble-accent') || '#0E9AA7').trim(),
            ];
        }

        var target = document.querySelector(appId === 'chess' ? '.chess-board' : '.ttt-grid');
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
        if (!session || session.status !== 'completed' || !_isMe(session, session.winner)) return;
        if (!session.game_id || _celebratedWins[session.game_id]) return;

        _celebratedWins[session.game_id] = true;
        if (session.game_id === _selectedSessionId && typeof haptic === 'function') {
            haptic(30);
            setTimeout(function() { haptic(30); }, 100);
            setTimeout(function() { haptic(15); }, 250);
        }
        if (typeof currentView !== 'undefined' && currentView === 'games' && typeof rsConfetti === 'function') {
            rsConfetti(_celebrationOptions(session));
        }
    }

    function _statusText(session) {
        var status = session.status;

        if (status === 'pending') {
            if (_isMe(session, session.challenger)) {
                // LXMF Direct's MAX_DELIVERY_ATTEMPTS=5 handles transient
                // wire loss; `failed` here means the transport gave up.
                // Resend is exposed via the `Resend last move` button.
                switch (session.delivery_state) {
                    case 'sending':          return 'Sending…';
                    case 'sent':
                    case 'routing':
                    case 'propagating':      return 'Sent';
                    case 'propagated':       return 'Stored';
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
            var t = session.terminal || '';
            if (t === 'draw') return 'Draw';
            if (t === 'resign') {
                return _isMe(session, session.winner) ? 'They resigned' : 'You resigned';
            }
            if (_isMe(session, session.winner)) return 'You won!';
            if (session.winner) return 'You lost!';
            return 'Completed';
        }
        if (status === 'active') {
            // In-flight/failed outbound move overrides the "their turn" label.
            if (session.delivery_state === 'failed') return 'Move failed — tap Resend';
            if (session.draw_offered) return 'Draw offered';
            var isChess = (session.app_id === 'chess' || session.game === 'chess');
            var myMarker, theirMarker;
            if (isChess) {
                var myCol = session.my_color || (session.metadata && session.metadata.my_color) || '';
                myMarker = myCol === 'b' ? 'Black' : 'White';
                theirMarker = myCol === 'b' ? 'White' : 'Black';
            } else {
                myMarker = session.my_marker
                    || (_isMe(session, session.first_turn) ? 'X' : 'O');
                theirMarker = myMarker === 'X' ? 'O' : 'X';
            }
            if (_isMe(session, session.turn)) return 'Your turn (' + myMarker + ')';
            if (session.turn) {
                var name = _contactName(session.contact_hash) || 'Opponent';
                return name + '\u2019s turn (' + theirMarker + ')';
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
            if (session.draw_offered) return 'status-challenge';
            return _isMe(session, session.turn) ? 'status-your-turn' : 'status-their-turn';
        }
        if (status === 'completed') {
            if (_isMe(session, session.winner)) return 'status-won';
            if (session.terminal === 'draw') return 'status-draw';
            return 'status-lost';
        }
        return 'status-muted';
    }

    function _gameIcon(appId) {
        if (appId === 'ttt') return '#';
        if (appId === 'chess') return '\u265E'; // black knight glyph — consistent across platforms
        return '?';
    }

    function _gameName(appId) {
        if (appId === 'ttt') return 'Tic-Tac-Toe';
        if (appId === 'chess') return 'Chess';
        return appId || 'Unknown';
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
            var classes = 'games-session-row';
            if (isActive) classes += ' active';
            if (s.unread > 0 && !isActive) classes += ' unread';

            var appId = _appId(s);
            html += '<div class="' + classes + ' game-row-' + escapeHtml(appId || 'unknown') + '" data-session-id="' + escapeHtml(s.game_id) + '" role="button" tabindex="0">' +
                '<div class="games-session-icon">' + _gameIcon(s.app_id || s.game) + '</div>' +
                '<div class="games-session-info">' +
                    '<div class="games-session-name">' + escapeHtml(_contactName(s.contact_hash)) + '</div>' +
                    '<div class="games-session-meta">' +
                        '<span class="games-session-game">' + _gameName(appId) + '</span>' +
                        '<span class="games-session-status ' + _statusClass(s) + '">' + _statusText(s) + '</span>' +
                    '</div>' +
                '</div>' +
                '<div class="games-session-time">' + relativeTime(s.updated_at || s.last_action_at) + '</div>' +
                '<button type="button" class="games-session-delete" aria-label="Remove game from history" title="Remove from history">' +
                    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 6h18"/><path d="M8 6V4h8v2"/><path d="M19 6l-1 14H6L5 6"/><path d="M10 11v5"/><path d="M14 11v5"/></svg>' +
                '</button>' +
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

        if (isMobile()) {
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
        if (typeof haptic === 'function') haptic(10);
        if (typeof rsConfirm !== 'function') return;
        rsConfirm({
            title: 'Remove game?',
            message: 'Remove this game from your history? This only affects your local list.',
            confirmText: 'Remove',
            danger: true
        }).then(function(ok) {
            if (!ok) return;
            if (typeof haptic === 'function') haptic(15);
            _deleteSession(sessionId);
        });
    }

    function _deleteSession(sessionId) {
        RS.invoke('delete_game_session', { sessionId: sessionId }).catch(function() {});
        _removeSessionLocal(sessionId);
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
            if (window.innerWidth <= 768 &&
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

        RS.invoke('mark_game_read', { sessionId: sessionId }).catch(function() {});

        for (var i = 0; i < _allSessions.length; i++) {
            if (_allSessions[i].game_id === sessionId) {
                _allSessions[i].unread = 0;
                break;
            }
        }

        renderSessionList();
        renderDetail();
        updateGamesBadge();

        if (window.innerWidth <= 768) {
            RS.viewStack.push('game-detail', { meta: { sessionId: sessionId } });
            history.pushState({ view: 'games', detail: true }, '', '#games');
        }
    }

    function _getSelectedSession() {
        if (!_selectedSessionId) return null;
        for (var i = 0; i < _allSessions.length; i++) {
            if (_allSessions[i].game_id === _selectedSessionId) return _allSessions[i];
        }
        return null;
    }

    function _renderDetailMeta(session) {
        var appId = _appId(session);
        var chips = [];
        var moveCount = parseInt(session.move_count, 10);
        if (!isNaN(moveCount) && moveCount > 0) chips.push('Move ' + moveCount);

        if (appId === 'chess') {
            var myColor = session.my_color || (session.metadata && session.metadata.my_color) || '';
            if (myColor === 'w') chips.push('White');
            if (myColor === 'b') chips.push('Black');
            if (session.in_check || (session.metadata && session.metadata.in_check)) chips.push('Check');
            var lastMove = session.last_move || (session.metadata && session.metadata.last_move) || '';
            if (lastMove) chips.push(lastMove.slice(0, 2) + '\u2192' + lastMove.slice(2, 4));
        } else if (appId === 'ttt') {
            var marker = session.my_marker || (_isMe(session, session.first_turn) ? 'X' : 'O');
            if (marker) chips.push('You are ' + marker);
        }

        if (session.delivery_state === 'sending') chips.push('Sending');
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
        panel.setAttribute('data-game', appId);
        var status = session.status;
        var statusTxt = _statusText(session);
        var statusCls = _statusClass(session);
        var themeClass = appId === 'chess' ? 'games-theme-chess' : (appId === 'ttt' ? 'games-theme-ttt' : 'games-theme-unknown');

        var html = '';

        html += '<button class="mobile-back-btn games-back-btn" aria-label="Back to games list">' +
            '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="15 18 9 12 15 6"/></svg>' +
        '</button>';

        html += '<div class="games-detail-header ' + themeClass + '">' +
            '<div class="games-detail-heading">' +
                '<span class="games-detail-icon">' + _gameIcon(appId) + '</span>' +
                '<span class="games-detail-copy">' +
                    '<span class="games-detail-title">' + _gameName(appId) + '</span>' +
                    '<span class="games-detail-vs">vs ' + escapeHtml(_contactName(session.contact_hash)) + '</span>' +
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
        if (appId === 'ttt') {
            html += _renderTTTBoard(session);
        } else if (appId === 'chess') {
            html += _renderChessBoard(session);
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
                if (typeof haptic === 'function') haptic(15);
                resendBtn.disabled = true;
                resendBtn.textContent = 'Resending…';
                session.delivery_state = 'sending';
                renderSessionList();
                renderDetail();
                RS.invoke('resend_last_game_action', {
                    args: { session_id: session.game_id }
                }).catch(function(err) {
                    if (typeof showToast === 'function') {
                        var msg = (err && err.message) || 'Resend failed';
                        showToast(msg, 'toast-red', 4000);
                    }
                });
            });
        }

        _bindControlEvents(session);
        _bindTTTCellEvents(session);
        _bindChessSquareEvents(session);
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
            oPlayer = escapeHtml(_contactName(session.contact_hash)) + ' (O)';
        } else {
            xPlayer = escapeHtml(_contactName(session.contact_hash)) + ' (X)';
            oPlayer = 'You (O)';
        }

        var xTurnActive = (status === 'active') && session.turn && _isMe(session, session.first_turn) === _isMe(session, session.turn);
        var oTurnActive = (status === 'active') && session.turn && !xTurnActive;

        var html = '<div class="ttt-board-wrap">';

        html += '<div class="ttt-player-label' + (xTurnActive ? ' active-turn' : '') + '">' + xPlayer + '</div>';

        var markerClass = isMyTurn ? (iAmX ? ' my-marker-x' : ' my-marker-o') : '';
        html += '<div class="ttt-grid' + (isMyTurn ? ' your-turn' : '') + markerClass + '">';
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
            html += '<div class="' + classes + '" data-cell-index="' + i + '">' + display + '</div>';
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

    function _handleTTTMove(session, cellIndex) {
        var board = (session.state || '_________').split('');
        if (board[cellIndex] !== '_') return;

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
            haptic(25);
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
        }).catch(function() {});
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
        var opponentName = escapeHtml(_contactName(opponentHash));
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

                html += '<div class="' + classes.join(' ') + '" data-square="' + sq + '">' + coordHtml + pieceHtml + '</div>';
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

        wrap.querySelectorAll('.chess-promotion-option').forEach(function(btn) {
            btn.addEventListener('click', function(e) {
                e.stopPropagation();
                var piece = btn.getAttribute('data-piece');
                wrap.remove();
                _chessSelected[sid] = null;
                _sendChessMove(session, baseUci + piece);
            });
        });

        var dismiss = function(e) {
            if (wrap.contains(e.target)) return;
            document.removeEventListener('click', dismiss, true);
            document.removeEventListener('keydown', escDismiss, true);
            wrap.remove();
            _chessSelected[sid] = null;
            _sendChessMove(session, baseUci + 'q');
        };
        var escDismiss = function(e) {
            if (e.key !== 'Escape' && e.key !== 'Enter') return;
            document.removeEventListener('click', dismiss, true);
            document.removeEventListener('keydown', escDismiss, true);
            wrap.remove();
            _chessSelected[sid] = null;
            _sendChessMove(session, baseUci + 'q');
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

        if (typeof haptic === 'function') haptic(25);

        RS.invoke('send_game_action', {
            args: {
                dest_hash: session.contact_hash,
                session_id: sid,
                app_id: 'chess',
                command: 'move',
                payload: { m: uci },
            }
        }).catch(function() {});
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
            var isChess = (session.app_id || session.game) === 'chess';
            if (session.draw_offered) {
                html += '<button class="nr-btn games-ctrl-accept" id="games-draw-accept-btn">Accept Draw</button>';
                html += '<button class="nr-btn nr-btn-secondary" id="games-draw-decline-btn">Decline Draw</button>';
                html += '<span class="games-ctrl-separator"></span>';
            } else if (isChess && session.draw_offer_reason === '3fr') {
                html += '<button class="nr-btn nr-btn-secondary" id="games-draw-offer-btn">Claim threefold</button>';
            } else if (isChess && session.draw_offer_reason === '50m') {
                html += '<button class="nr-btn nr-btn-secondary" id="games-draw-offer-btn">Claim 50-move</button>';
            } else {
                html += '<button class="nr-btn nr-btn-secondary" id="games-draw-offer-btn">Offer Draw</button>';
            }
            html += '<button class="nr-btn nr-btn-danger" id="games-resign-btn">Resign</button>';
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
            // FIDE reason makes the peer auto-accept instead of prompting.
            var payload = {};
            var isChess = (session.app_id || session.game) === 'chess';
            if (isChess && (session.draw_offer_reason === '3fr' || session.draw_offer_reason === '50m')) {
                payload = { r: session.draw_offer_reason };
            }
            _sendAction(session, 'draw_offer', payload);
        });
        _bindBtn('games-draw-accept-btn', function() {
            _sendAction(session, 'draw_accept');
        });
        _bindBtn('games-draw-decline-btn', function() {
            _sendAction(session, 'draw_decline');
        });
    }

    function _bindBtn(id, handler) {
        var el = document.getElementById(id);
        if (el) el.addEventListener('click', handler);
    }

    function _sendAction(session, action, payload) {
        RS.invoke('send_game_action', {
            args: {
                dest_hash: session.contact_hash,
                session_id: session.game_id,
                app_id: session.app_id || session.game || 'ttt',
                command: action,
                payload: payload || {},
            }
        }).then(function(ack) {
            if (ack && ack.ok === false) {
                var msg = _reasonToMessage(ack.reason || 'send_failed', action);
                if (typeof showToast === 'function') showToast(msg, 'toast-red', 4000);
            }
        }).catch(function() {
            if (typeof showToast === 'function') {
                showToast(_reasonToMessage('send_failed', action), 'toast-red', 4000);
            }
        });
    }

    function _reasonToMessage(reason, command) {
        switch (reason) {
            case 'invalid_params':       return 'Bad action parameters';
            case 'session_terminal':     return 'Session already ended';
            case 'dispatch_failed':      return 'Action rejected by game rules';
            case 'not_your_turn':        return 'Not your turn';
            case 'lxmf_not_initialized': return 'Messaging not ready — wait a moment';
            case 'pack_failed':          return 'Action rejected — invalid envelope';
            case 'send_failed':
            default:
                return command === 'move'
                    ? 'Move couldn’t be delivered — tap Resend'
                    : 'Action couldn’t be delivered';
        }
    }

    function showNewGameDialog() {
        _showNewGameSheet();
    }

    function _showNewGameSheet() {
        if (typeof haptic === 'function') haptic(10);

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
            contactsHtml = '<div class="games-sheet-empty">No contacts yet.</div>';
        } else {
            for (var i = 0; i < sorted.length; i++) {
                var c = sorted[i];
                var name = c.display_name || 'Anonymous';
                var avatar = (typeof identityAvatar === 'function') ? identityAvatar(c.hash, 32) : '';
                contactsHtml += '<button type="button" class="games-sheet-contact-row" data-hash="' + escapeHtml(c.hash) + '" aria-pressed="false">' +
                    '<span class="games-sheet-contact-avatar">' + avatar + '</span>' +
                    '<span class="games-sheet-contact-copy">' +
                        '<span class="games-sheet-contact-name">' + escapeHtml(name) + '</span>' +
                        '<span class="games-sheet-contact-hash">' + escapeHtml(_shortHash(c.hash, 8, 4)) + '</span>' +
                    '</span>' +
                '</button>';
            }
        }

        var overlayHtml = '<div class="bottom-sheet-overlay" id="games-new-sheet-overlay"></div>';
        var sheetHtml = '<div class="bottom-sheet games-new-dialog" id="games-new-sheet">' +
            '<div class="bottom-sheet-handle"></div>' +
            '<div class="bottom-sheet-header">' +
                '<div>' +
                    '<div class="bottom-sheet-title">New game</div>' +
                    '<div class="games-sheet-subtitle">Choose a game and opponent.</div>' +
                '</div>' +
                '<button type="button" class="bottom-sheet-close" id="games-sheet-close" aria-label="Close">&times;</button>' +
            '</div>' +
            '<div class="bottom-sheet-body">' +
                '<div class="games-sheet-section">' +
                    '<div class="games-sheet-header">Game</div>' +
                    '<div class="games-sheet-game-grid">' +
                        '<button type="button" class="games-sheet-game-card selected" data-app-id="ttt" aria-pressed="true">' +
                            '<span class="game-card-icon">#</span>' +
                            '<span><span class="games-sheet-game-name">Tic-Tac-Toe</span><span class="games-sheet-game-hint">Fast, simple turns</span></span>' +
                        '</button>' +
                        '<button type="button" class="games-sheet-game-card" data-app-id="chess" aria-pressed="false">' +
                            '<span class="game-card-icon">\u265E</span>' +
                            '<span><span class="games-sheet-game-name">Chess</span><span class="games-sheet-game-hint">Full rules</span></span>' +
                        '</button>' +
                    '</div>' +
                '</div>' +
                '<div class="games-sheet-section">' +
                    '<div class="games-sheet-header">Opponent</div>' +
                    '<div class="games-sheet-contact-list">' + contactsHtml + '</div>' +
                '</div>' +
            '</div>' +
            '<div class="bottom-sheet-footer games-sheet-footer">' +
                '<button type="button" class="games-sheet-cancel-btn" id="games-sheet-cancel">Cancel</button>' +
                '<button type="button" class="games-sheet-send-btn" id="games-sheet-send" disabled>Send Challenge</button>' +
            '</div>' +
        '</div>';

        document.body.insertAdjacentHTML('beforeend', overlayHtml);
        document.body.insertAdjacentHTML('beforeend', sheetHtml);

        var overlay = document.getElementById('games-new-sheet-overlay');
        var sheet = document.getElementById('games-new-sheet');

        requestAnimationFrame(function() {
            if (overlay) overlay.classList.add('active');
            if (sheet) sheet.classList.add('open');
        });

        var selectedHash = null;
        var selectedAppId = 'ttt';

        if (sheet) {
            sheet.querySelectorAll('.games-sheet-game-card').forEach(function(card) {
                card.addEventListener('click', function() {
                    if (typeof haptic === 'function') haptic(10);
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
                    if (typeof haptic === 'function') haptic(10);
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
        }

        _bindBtn('games-sheet-close', _closeNewGameSheet);
        _bindBtn('games-sheet-cancel', _closeNewGameSheet);
        _bindBtn('games-sheet-send', function() {
            if (!selectedHash) return;
            if (typeof haptic === 'function') haptic(10);
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

    function _closeNewGameSheet() {
        var overlay = document.getElementById('games-new-sheet-overlay');
        var sheet = document.getElementById('games-new-sheet');
        if (overlay) overlay.classList.remove('active');
        if (sheet) {
            sheet.classList.remove('open');
            setTimeout(function() {
                if (overlay) overlay.remove();
                if (sheet) sheet.remove();
            }, 300);
        } else if (overlay) {
            overlay.remove();
        }
    }

    function startNewGame(appId, contactHash) {
        var arr = new Uint8Array(8);
        crypto.getRandomValues(arr);
        var sessionId = '';
        for (var i = 0; i < arr.length; i++) {
            sessionId += ('0' + arr[i].toString(16)).slice(-2);
        }

        RS.invoke('send_game_action', {
            args: {
                dest_hash: contactHash,
                session_id: sessionId,
                app_id: appId,
                command: 'challenge',
                payload: {},
            }
        }).then(function(ack) {
            if (ack && ack.ok === false) {
                var msg = _reasonToMessage(ack.reason || 'send_failed', 'challenge');
                if (typeof showToast === 'function') showToast('Challenge failed: ' + msg, 'toast-red', 4000);
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

        // Success clears the optimistic backup; failure restores it.
        RS.listen('game_action_result', function(data) {
            if (!data || !data.session_id) return;
            var sid = data.session_id;

            if (data.ok === true) {
                delete _optimisticBackup[sid];
                return;
            }

            var reason = data.reason || 'send_failed';

            // Construction-time send failure (lxmf_not_initialized, hex/sign).
            // Backend already rolled back; drop the optimistic backup so we
            // don't try to double-rollback locally.
            if (reason === 'send_failed') {
                delete _optimisticBackup[sid];
                if (typeof showToast === 'function') {
                    showToast(_reasonToMessage(reason, data.command), 'toast-red', 5000);
                }
                if (typeof haptic === 'function') haptic(40);
                return;
            }

            // Immediate rejection — backend never mutated, roll back locally.
            var backup = _optimisticBackup[sid];
            if (!backup) {
                if (typeof showToast === 'function') {
                    showToast(_reasonToMessage(reason, data.command), 'toast-red', 4000);
                }
                return;
            }

            for (var i = 0; i < _allSessions.length; i++) {
                if (_allSessions[i].game_id !== sid) continue;
                var s = _allSessions[i];
                s.state = backup.state;
                s.move_count = backup.move_count;
                s.turn = backup.turn;
                s.status = backup.status;
                s.terminal = backup.terminal;
                s.winner = backup.winner;
                s.delivery_state = backup.delivery_state;
                if (backup.fen !== undefined) s.fen = backup.fen;
                if (backup.legal_moves !== undefined) s.legal_moves = backup.legal_moves;
                if (backup.last_move !== undefined) s.last_move = backup.last_move;
                if (backup.in_check !== undefined) s.in_check = backup.in_check;
                if (backup.draw_offer_reason !== undefined) s.draw_offer_reason = backup.draw_offer_reason;
                if (backup.terminal_reason !== undefined) s.terminal_reason = backup.terminal_reason;
                break;
            }
            delete _optimisticBackup[sid];

            renderSessionList();
            if (sid === _selectedSessionId) renderDetail();

            if (typeof showToast === 'function') {
                showToast(_reasonToMessage(reason, data.command), 'toast-red', 4000);
            }
            if (typeof haptic === 'function') haptic(40);
        });

        // Per-action signal from the runtime — forces a board redraw and badge
        // refresh even if the bulk `all_game_sessions` payload arrives stale or
        // the listener registration raced with Tauri global injection.
        RS.listen('game_action_received', function(data) {
            if (!data || !data.session_id) return;
            if (data.session_id === _selectedSessionId) renderDetail();
            updateGamesBadge();
        });

        // Initial sync runs via gamesTabLoad() on first Games view activation.
        _gameEventsReady = true;
    }

    function _handleSessionDelta(record, prev) {
        if (!record || !record.game_id) return;

        var prevBoard = prev ? prev.state : null;
        var prevStatus = prev ? prev.status : null;

        if (record.game_id === _selectedSessionId && record.state && prevBoard && record.state !== prevBoard) {
            for (var c = 0; c < 9; c++) {
                if ((prevBoard[c] || '_') !== (record.state[c] || '_')) {
                    _animatingCell = c;
                    _animatingCellExpiry = Date.now() + 600;
                    break;
                }
            }
        }

        var isNew = !prev;
        if (isNew && typeof currentView !== 'undefined' && currentView !== 'games') {
            if (record.status === 'pending' && !_isMe(record, record.challenger)) {
                if (typeof showToast === 'function') showToast('\uD83C\uDFAE Game challenge from ' + _contactName(record.contact_hash), 'toast-green', 5000);
                if (typeof haptic === 'function') { haptic(10); setTimeout(function() { haptic(10); }, 80); }
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
        var movedSinceLast = prev && record.move_count !== prev.move_count;
        var notViewingThisGame = (typeof currentView === 'undefined' || currentView !== 'games')
            || record.game_id !== _selectedSessionId;
        if (movedSinceLast && notViewingThisGame && record.status === 'active') {
            if (typeof showToast === 'function') showToast('Game update from ' + _contactName(record.contact_hash), 'toast-blue', 3000);
            if (typeof haptic === 'function') haptic(15);
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
            if (_allSessions[i].unread > 0) total++;
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
        _bindBtn('games-fab-btn', function() {
            if (typeof haptic === 'function') haptic(10);
            showNewGameDialog();
        });
    }

    window.gamesTabLoad = function() {
        _contactNameCache = {};
        RS.invoke('get_all_game_sessions').then(function(sessions) {
            if (Array.isArray(sessions)) {
                _allSessions = sessions;
                renderSessionList();
            }
        }).catch(function() {});
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

    function _init() {
        _initTabFilters();
        _initNewGameBtn();
        _initGameEvents();
    }

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', _init);
    } else {
        _init();
    }

})();
