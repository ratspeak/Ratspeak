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
    // An ending that arrived this run shows as a card over the board until dismissed.
    var _freshEndings = {};
    var _resultShown = {};
    var _resultSheet = null;

    var WIN_LINES = [
        [0,1,2],[3,4,5],[6,7,8],
        [0,3,6],[1,4,7],[2,5,8],
        [0,4,8],[2,4,6]
    ];

    var FILTERS = ['all', 'active', 'pending', 'done'];
    var GAME_ORDER = ['chess', 'four_in_a_row', 'ttt'];
    var GAME_DESCRIPTIONS = {
        chess: 'The classic. Play over days; a move can wait a week.',
        four_in_a_row: 'Drop pieces into columns. First to line up four wins.',
        ttt: 'Three in a row on nine squares. The quickest game.'
    };
    var CHESS_REASONS = { cm: 'checkmate', sm: 'stalemate', ins: 'insufficient material', '3fr': 'threefold repetition', '50m': 'fifty-move rule', agr: 'by agreement' };
    var NUMBER_WORDS = ['zero', 'one', 'two', 'three', 'four', 'five', 'six', 'seven', 'eight', 'nine', 'ten'];

    function _icon(name) {
        return RS.kit && typeof RS.kit.icon === 'function' ? RS.kit.icon(name) : '';
    }

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

    // "Ada Okafor" reads "Ada" mid-sentence.
    function _firstName(hash) {
        var name = _contactName(hash) || 'Opponent';
        return name.split(/\s+/)[0] || name;
    }

    function _lastHeard(hash) {
        if (typeof PeersCache !== 'undefined' && PeersCache && typeof PeersCache.get === 'function') {
            var peer = PeersCache.get(hash);
            if (peer && peer.last_seen) return peer.last_seen;
        }
        var contacts = _getContacts();
        for (var i = 0; i < contacts.length; i++) {
            if (contacts[i].hash === hash) return contacts[i].last_seen || 0;
        }
        return 0;
    }

    // Lists show presence only while it is active.
    function _avatar(hash, size) {
        var heard = _lastHeard(hash);
        var active = RS.kit && RS.kit.presenceFor(heard) === 'active' && heard;
        return RS.kit.avatar(hash, size, active ? 'active' : undefined, active ? heard : undefined);
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
        if (typeof RS !== 'undefined' && typeof RS.isAttentionForeground === 'function' &&
            !RS.isAttentionForeground()) {
            return false;
        }
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
            ? document.querySelector('#games-detail ' + view.boardSelector)
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

    // A win, loss, or draw that arrives while Ratspeak runs gets the result card.
    function _noteEnding(session) {
        if (!session || session.status !== 'completed' || !session.game_id) return;
        _freshEndings[session.game_id] = true;
    }

    // Today's confetti, only for a new win and only once the board is on screen.
    function _maybeCelebrateWin(session) {
        if (!session || session.status !== 'completed' || _outcome(session) !== 'won') return;
        if (!session.game_id || _celebratedWins[session.game_id] || !_freshEndings[session.game_id]) return;
        if (!_isViewingSession(session.game_id)) return;

        _celebratedWins[session.game_id] = true;
        if (typeof haptic === 'function') haptic('success');
        if (typeof rsConfetti === 'function') rsConfetti(_celebrationOptions(session));
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
        if (state === 'failed') return 'Move not delivered';
        return '';
    }

    // 'won', 'lost', 'draw', or '' for a game that is not finished.
    function _outcome(session) {
        if (!session || session.status !== 'completed') return '';
        if (_sessionValue(session, 'terminal', '') === 'draw') return 'draw';
        var winner = _sessionValue(session, 'winner', '');
        if (_isMe(session, winner)) return 'won';
        if (winner) return 'lost';
        return 'ended';
    }

    function _isResignation(session) {
        return _sessionValue(session, 'terminal', '') === 'resign' ||
            _sessionValue(session, 'terminal_reason', '') === 'rsn';
    }

    // Plain status words for rows and the line under a finished board.
    function _statusText(session) {
        var status = session.status;
        var them = _firstName(session.contact_hash);

        if (status === 'pending') {
            if (_isMe(session, session.challenger)) {
                // LXMF Direct's MAX_DELIVERY_ATTEMPTS=5 handles transient
                // wire loss; `failed` here means the transport gave up.
                // Resend is exposed in the challenge detail.
                if (_isSendingDeliveryState(session.delivery_state) || session.delivery_state === 'pending') {
                    return 'Sending challenge…';
                }
                switch (session.delivery_state) {
                    case 'propagating':      return 'Storing in Offline Inbox…';
                    case 'propagated':       return 'Stored in Offline Inbox';
                    case 'failed':           return 'Challenge not delivered';
                    default:                 return 'Waiting for ' + them + ' to accept';
                }
            }
            return 'Challenged you';
        }
        if (status === 'declined') {
            if (session.cancelled_by_initiator) {
                return _isMe(session, session.challenger) ? 'You cancelled' : 'Challenge cancelled';
            }
            return _isMe(session, session.challenger) ? them + ' declined' : 'You declined';
        }
        if (status === 'expired') return 'Expired';
        if (status === 'completed') {
            var outcome = _outcome(session);
            if (outcome === 'draw') return 'Draw';
            if (outcome === 'ended') return 'Finished';
            if (_isResignation(session)) return outcome === 'won' ? them + ' resigned' : 'You resigned';
            return outcome === 'won' ? 'You won' : them + ' won';
        }
        if (status === 'active') {
            // In-flight/failed outbound move overrides the "their turn" label.
            var deliveryText = _activeMoveDeliveryText(session.delivery_state);
            if (deliveryText) return deliveryText;
            if (_sessionValue(session, 'draw_offered', false)) {
                return _isMe(session, _drawOfferOwner(session))
                    ? 'Draw offer sent'
                    : them + ' offers a draw';
            }
            var turn = _sessionValue(session, 'turn', '');
            if (_isMe(session, turn)) return 'Your turn';
            if (turn) return them + '’s turn';
            return 'Active';
        }
        return status;
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

    // The game's board art in a tile; a neutral games tile for a game without art.
    function _thumb(appId, html, large, extraClass) {
        var art = html || (RS.games.art ? RS.games.art.preview(appId, 'square') : '');
        var cls = 'game-thumb' + (large ? ' game-thumb--lg' : '') + (extraClass ? ' ' + extraClass : '');
        if (!art) {
            return '<span class="ui-tile ui-tile--neutral' + (large ? ' ui-tile--lg' : '') +
                (extraClass ? ' ' + extraClass : '') + '" aria-hidden="true">' + _icon('games') + '</span>';
        }
        return '<span class="' + cls + '" aria-hidden="true">' + art + '</span>';
    }

    function _positionThumb(session) {
        return _thumb(_appId(session), RS.games.art ? RS.games.art.position(session) : '', true);
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

    function _isDone(session) {
        var status = session.status;
        return status === 'completed' || status === 'declined' || status === 'expired';
    }

    function _filterSessions() {
        if (_activeFilter === 'all') return _allSessions;
        return _allSessions.filter(function(s) {
            if (_activeFilter === 'active') return s.status === 'active';
            if (_activeFilter === 'pending') return s.status === 'pending';
            if (_activeFilter === 'done') return _isDone(s);
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
        return !!session && _isDone(session);
    }

    // Games waiting on you: your move, or a challenge to answer.
    function _needsMe(session) {
        if (session.status === 'pending') return !_isMe(session, session.challenger);
        if (session.status !== 'active') return false;
        if (_sessionValue(session, 'draw_offered', false) &&
                !_isMe(session, _drawOfferOwner(session))) return true;
        return !!_isMe(session, _sessionValue(session, 'turn', ''));
    }

    function _sessionTime(session) {
        return session.last_action_at || session.updated_at || session.created_at || 0;
    }

    function _timeHtml(ts) {
        if (!ts) return '';
        return '<span class="ui-time" title="' + escapeHtml(RS.kit.time.full(ts)) + '">' +
            escapeHtml(RS.kit.time.row(ts)) + '</span>';
    }

    function _sessionRow(s) {
        var isSelected = s.game_id === _selectedSessionId &&
            !(typeof isCompactLayout === 'function' && isCompactLayout());
        var unread = s.unread > 0 && !_isViewingSession(s.game_id);
        var appId = _appId(s);
        return '<li><button type="button" class="ui-row' + (unread ? ' is-unread' : '') + '"' +
            (isSelected ? ' aria-current="true"' : '') + ' data-session-id="' + escapeHtml(s.game_id) + '">' +
            _thumb(appId) +
            '<span class="ui-row-main">' +
                '<span class="ui-row-top"><span class="ui-row-title"><bdi dir="auto">' +
                    escapeHtml(_contactName(s.contact_hash)) + '</bdi></span>' + _timeHtml(_sessionTime(s)) + '</span>' +
                '<span class="ui-row-sub"><span class="t">' +
                    escapeHtml(_gameName(appId) + ' · ' + _statusText(s)) + '</span>' +
                    (unread ? '<span class="ui-count" aria-label="' + s.unread + ' new">' +
                        (s.unread > 99 ? '99+' : s.unread) + '</span>' : '') +
                '</span>' +
            '</span></button></li>';
    }

    function renderSessionList() {
        var container = document.getElementById('games-session-list');
        if (!container) return;
        var first = _allSessions.length === 0;
        var pane = container.closest ? container.closest('.games-pane') : null;
        if (pane) pane.classList.toggle('is-first', first);
        var filter = document.getElementById('games-filter');
        if (filter) filter.hidden = first;

        if (first) {
            // Desktop keeps one quiet line here (the landing fills the detail); a phone shows the landing itself.
            container.innerHTML =
                '<div class="ui-empty ui-empty--quiet ui-desk-only"><h3 class="ui-empty-title">No games yet</h3>' +
                    '<p class="ui-empty-body">Games you start or are challenged to show here.</p></div>' +
                '<div class="ui-phone-only">' + _renderLanding(true, true) + '</div>';
            _bindNewGameTriggers(container);
            return;
        }

        var filtered = _filterSessions();
        if (filtered.length === 0) {
            container.innerHTML = '<div class="ui-empty ui-empty--quiet"><h3 class="ui-empty-title">' +
                escapeHtml(_emptyFilterTitle()) + '</h3></div>';
            return;
        }

        var html = '<ul class="ui-list">';
        for (var i = 0; i < filtered.length; i++) html += _sessionRow(filtered[i]);
        container.innerHTML = html + '</ul>';

        var rows = container.querySelectorAll('.ui-row[data-session-id]');
        for (var j = 0; j < rows.length; j++) {
            _bindSessionRow(rows[j]);
        }
    }

    function _emptyFilterTitle() {
        if (_activeFilter === 'active') return 'No games in progress';
        if (_activeFilter === 'pending') return 'No challenges waiting';
        if (_activeFilter === 'done') return 'No finished games';
        return 'No games yet';
    }

    function _bindSessionRow(row) {
        var sessionId = row.getAttribute('data-session-id');

        row.addEventListener('click', function() {
            selectSession(sessionId);
        });

        if (isMobile() && _canDeleteSession(_findSession(sessionId))) {
            var firedRecently = false;
            RS.gestures.attachLongPress(row, {
                duration: RS.gestures.LONG_PRESS_GAMES_ROW_MS,
                moveCancelPx: RS.gestures.LONG_PRESS_MOVE_CANCEL_PX,
                hapticStages: [{ at: 1.0, level: 'medium' }],
                onFire: function() {
                    firedRecently = true;
                    _confirmDeleteSession(sessionId);
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

    function _confirmDeleteSession(sessionId) {
        if (!sessionId) return;
        var session = _findSession(sessionId);
        if (!_canDeleteSession(session)) {
            if (typeof showToast === 'function') {
                showToast('Finish the game before removing it', 'toast-warning', 3000);
            }
            return;
        }
        if (typeof rsConfirm !== 'function') {
            if (typeof showToast === 'function') {
                showToast('Confirmation dialog unavailable', 'toast-error', 3000);
            }
            return;
        }
        rsConfirm({
            title: 'Remove this game?',
            message: 'The game is removed from this device. ' + _contactName(session.contact_hash) +
                ' keeps their copy.',
            confirmText: 'Remove game',
            cancelText: 'Cancel',
            danger: true,
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
                showToast('Game could not be removed', 'toast-error', 3000);
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
        delete _freshEndings[sessionId];
        delete _resultShown[sessionId];
        if (_resultSheet && _resultSheet.sessionId === sessionId) _closeResultSheet();
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
        var session = _findSession(sessionId);
        // An ending that landed while you were away still gets its card once.
        if (session && session.status === 'completed' && session.unread > 0) _noteEnding(session);
        _selectedSessionId = sessionId;
        _markSessionReadLocal(sessionId, { render: false, force: true });

        if (isCompactLayout()) {
            var top = RS.viewStack.top();
            if (top && top.viewId === 'game-detail') {
                // A rematch from an open game replaces the board instead of stacking a second one.
                top.opts = { meta: { sessionId: sessionId } };
            } else {
                RS.viewStack.push('game-detail', { meta: { sessionId: sessionId } });
                history.pushState({ view: 'games', detail: true }, '', '#games');
            }
        }

        renderSessionList();
        renderDetail();
        updateGamesBadge();
    }

    function _getSelectedSession() {
        return _selectedSessionId ? _findSession(_selectedSessionId) : null;
    }

    // Landing: Your move, Start a new game, and Play again. A first run shows the start only.
    function _renderLanding(first, phoneFirst) {
        var waiting = _allSessions.filter(_needsMe);
        var html = '<div class="game-landing">';
        html += '<header class="game-landing-head"><div>' +
            (phoneFirst ? '' : '<h2 class="ui-detail-title">' + (first ? 'Play a game' : 'Play') + '</h2>') +
            '<p class="game-lead">' + (first
                ? 'Challenge a contact to chess, four in a row, or tic-tac-toe. Moves travel over the mesh, so a game can last minutes or days.'
                : 'Pick up a game, or challenge someone to a new one.') + '</p></div>' +
            (phoneFirst ? '' : '<button type="button" class="ui-btn ui-btn--primary" data-new-game="">' +
                _icon('plus') + 'New game</button>') + '</header>';
        if (phoneFirst) {
            html += '<button type="button" class="ui-btn ui-btn--primary ui-btn--lg ui-btn--block" data-new-game="">New game</button>';
        }

        if (!first && !phoneFirst && waiting.length) {
            html += '<section class="game-section" aria-labelledby="game-landing-yours"><h3 class="ui-section-title" id="game-landing-yours">Your move</h3><ul class="ui-list">';
            waiting.forEach(function(s) {
                var appId = _appId(s);
                var sub = s.status === 'pending'
                    ? 'Wants to play. Accept or decline.'
                    : (_sessionValue(s, 'draw_offered', false) ? _statusText(s) : 'Your turn' +
                        (_moveNumber(s) ? ', move ' + _moveNumber(s) : ''));
                html += '<li><button type="button" class="ui-row" data-open-session="' + escapeHtml(s.game_id) + '">' +
                    _positionThumb(s) +
                    '<span class="ui-row-main"><span class="ui-row-top"><span class="ui-row-title"><bdi dir="auto">' +
                        escapeHtml(_contactName(s.contact_hash)) + '</bdi></span>' + _timeHtml(_sessionTime(s)) + '</span>' +
                    '<span class="ui-row-sub"><span class="t">' + escapeHtml(_gameName(appId) + ' · ' + sub) + '</span></span></span>' +
                    _icon('chevron-right') + '</button></li>';
            });
            html += '</ul></section>';
        }

        html += '<section class="game-section" aria-labelledby="game-landing-new"><h3 class="ui-section-title" id="game-landing-new">' +
            (first ? 'Choose a game' : 'Start a new game') + '</h3><div class="game-tiles">';
        _newGameManifests().forEach(function(manifest) {
            var appId = manifest.app_id;
            var desc = GAME_DESCRIPTIONS[appId] || '';
            html += '<button type="button" class="game-choice" data-new-game="' + escapeHtml(appId) + '"' +
                (desc ? ' aria-describedby="game-tile-' + escapeHtml(appId) + '"' : '') + '>' +
                '<span class="game-art" aria-hidden="true">' + (RS.games.art ? RS.games.art.preview(appId, 'wide') : '') + '</span>' +
                '<span class="game-choice-copy"><span class="game-choice-top"><span class="game-choice-name">' +
                    escapeHtml(manifest.display_name || appId) + '</span></span>' +
                    (desc ? '<span class="game-choice-desc" id="game-tile-' + escapeHtml(appId) + '">' + escapeHtml(desc) + '</span>' : '') +
                '</span></button>';
        });
        html += '</div></section>';

        var again = first || phoneFirst ? [] : _playAgain(waiting);
        if (again.length) {
            html += '<section class="game-section" aria-labelledby="game-landing-again"><h3 class="ui-section-title" id="game-landing-again">Play again</h3><ul class="game-people">';
            again.forEach(function(s) {
                var outcome = _outcome(s);
                var sub = _gameName(_appId(s)) + (outcome === 'won' ? ' · you won'
                    : outcome === 'lost' ? ' · you lost' : outcome === 'draw' ? ' · draw' : '');
                html += '<li><button type="button" class="ui-row" data-new-game="' + escapeHtml(_appId(s)) +
                    '" data-opponent="' + escapeHtml(s.contact_hash) + '">' + _avatar(s.contact_hash, 32) +
                    '<span class="ui-row-main"><span class="ui-row-title"><bdi dir="auto">' + escapeHtml(_contactName(s.contact_hash)) +
                    '</bdi></span><span class="ui-row-sub"><span class="t">' + escapeHtml(sub) + '</span></span></span></button></li>';
            });
            html += '</ul></section>';
        }
        return html + '</div>';
    }

    // Recent opponents, newest first, skipping anyone with a game or challenge waiting on either side.
    function _playAgain(waiting) {
        var skip = {};
        waiting.forEach(function(s) { skip[s.contact_hash] = true; });
        _allSessions.forEach(function(s) { if (s.status === 'pending') skip[s.contact_hash] = true; });
        var seen = {};
        return _allSessions.slice().sort(function(a, b) {
            return _sessionTime(b) - _sessionTime(a);
        }).filter(function(s) {
            if (!s.contact_hash || skip[s.contact_hash] || seen[s.contact_hash]) return false;
            seen[s.contact_hash] = true;
            return true;
        }).slice(0, 3);
    }

    function _bindNewGameTriggers(root) {
        var triggers = root.querySelectorAll('[data-new-game]');
        for (var i = 0; i < triggers.length; i++) {
            triggers[i].addEventListener('click', function() {
                showNewGameDialog({
                    game: this.getAttribute('data-new-game') || null,
                    opponent: this.getAttribute('data-opponent') || null,
                });
            });
        }
        var opens = root.querySelectorAll('[data-open-session]');
        for (var j = 0; j < opens.length; j++) {
            opens[j].addEventListener('click', function() {
                selectSession(this.getAttribute('data-open-session'));
            });
        }
    }

    // Chess counts full moves from its FEN; the other games show no move number.
    function _moveNumber(session) {
        if (_appId(session) !== 'chess' || session.status !== 'active') return 0;
        var fen = _sessionValue(session, 'fen', '');
        var parts = String(fen || '').split(' ');
        var full = parseInt(parts[5], 10);
        return full > 0 ? full : 0;
    }

    function _detailHeader(session, statusHtml) {
        var appId = _appId(session);
        var move = _moveNumber(session);
        return '<header class="game-head ui-scroll-head">' +
            '<button type="button" class="ui-icon-btn ui-phone-only games-back-btn" aria-label="Back to games" title="Back">' + _icon('chevron-left') + '</button>' +
            _thumb(appId, '', true, 'ui-desk-only') +
            '<div class="game-head-main"><h2 class="ui-detail-title"><bdi dir="auto">' + escapeHtml(_contactName(session.contact_hash)) + '</bdi></h2>' +
                '<span class="game-head-sub">' + escapeHtml(_gameName(appId) + (move ? ' · move ' + move : '')) + '</span></div>' +
            (statusHtml || '') + '</header>';
    }

    function _deliveryNotice(session, text) {
        return '<div class="ui-notice ui-notice--warning game-notice" role="status">' + _icon('alert') +
            '<div class="ui-notice-body">' + escapeHtml(text) + '</div>' +
            '<button type="button" class="ui-btn ui-btn--sm games-resend-btn">Resend</button></div>';
    }

    function _untilText(ts) {
        var ms = ts < 1e11 ? ts * 1000 : ts;
        var then = new Date(ms), now = new Date();
        var days = Math.round((new Date(then.getFullYear(), then.getMonth(), then.getDate()) -
            new Date(now.getFullYear(), now.getMonth(), now.getDate())) / 86400000);
        var clock = RS.kit.time.clock(ms);
        if (days <= 0) return clock + ' today';
        if (days === 1) return clock + ' tomorrow';
        return RS.kit.time.date(ms) + ' at ' + clock;
    }

    // A challenge: the game's art, one sentence, and its one decision.
    function _renderInvite(session) {
        var appId = _appId(session);
        var them = escapeHtml(_firstName(session.contact_hash));
        var game = escapeHtml(_gameName(appId));
        var art = '<span class="game-art" aria-hidden="true">' + (RS.games.art ? RS.games.art.preview(appId, 'wide') : '') + '</span>';
        var html = '<div class="game-invite">' + art;

        if (session.status === 'pending' && !_isMe(session, session.challenger)) {
            var firstTurn = _sessionValue(session, 'first_turn', '');
            var mover = !firstTurn ? 'You can take your time.'
                : (_isMe(session, firstTurn) ? 'You move first, and you can take your time.'
                    : them + ' moves first, and you can take your time.');
            html += '<h3>' + them + ' challenged you to ' + game + '</h3>' +
                '<p>Accept to start. ' + mover + '</p>' +
                '<div class="game-controls">' +
                    '<button type="button" class="ui-btn ui-btn--quiet ui-btn--pair" id="games-decline-btn">Decline</button>' +
                    '<button type="button" class="ui-btn ui-btn--primary ui-btn--pair" id="games-accept-btn">Accept</button>' +
                '</div>';
        } else if (session.status === 'pending') {
            var sentence = 'Your challenge to ' + game.toLowerCase();
            if (session.delivery_state === 'delivered') {
                sentence += ' was delivered ' + escapeHtml(RS.kit.time.sentence(_sessionTime(session))) + '.';
            } else if (session.delivery_state === 'failed') {
                sentence += ' wasn’t delivered.';
            } else if (session.delivery_state === 'propagating') {
                sentence += ' is being stored in an Offline Inbox.';
            } else if (session.delivery_state === 'propagated') {
                sentence += ' is waiting in an Offline Inbox.';
            } else {
                sentence += ' is on its way.';
            }
            var ttl = _manifestsById[appId] && _manifestsById[appId].ttl;
            var pendingTtl = ttl && Number(ttl.pending);
            if (pendingTtl > 0 && session.delivery_state !== 'failed') {
                sentence += ' ' + them + ' has until ' + escapeHtml(_untilText(_sessionTime(session) + pendingTtl)) + ' to accept.';
            }
            html += '<h3>Waiting for ' + them + ' to accept</h3><p>' + sentence + '</p>' +
                (session.delivery_state === 'failed' ? _deliveryNotice(session, 'Your challenge wasn’t delivered.') : '') +
                '<div class="game-controls"><button type="button" class="ui-btn ui-btn--quiet" id="games-cancel-btn">Cancel challenge</button></div>';
        } else {
            // Declined or expired.
            html += '<h3>' + escapeHtml(_statusText(session)) + '</h3>' +
                '<p>' + (session.status === 'expired' ? 'Nobody moved in time, so the game closed.' : 'You can send a new challenge any time.') + '</p>' +
                _afterControls(session, false, false);
        }
        return html + '</div>';
    }

    function _playerRow(hash, name, meta, turnLabel, isYou) {
        return '<div class="game-player">' + (isYou ? RS.kit.avatar(hash, 32) : _avatar(hash, 32)) +
            '<span class="game-player-main"><span class="game-player-name"><bdi dir="auto">' + escapeHtml(name) + '</bdi></span>' +
                (meta ? '<span class="game-player-meta">' + escapeHtml(meta) + '</span>' : '') + '</span>' +
            (turnLabel ? '<span class="ui-status"><span class="ui-unread" aria-hidden="true"></span>' + escapeHtml(turnLabel) + '</span>' : '') +
            '</div>';
    }

    function _numberWord(n) {
        return n >= 0 && n < NUMBER_WORDS.length ? NUMBER_WORDS[n] : String(n);
    }

    function _boardFull(session) {
        var board = _appId(session) === 'ttt'
            ? (session.state || _sessionValue(session, 'board', ''))
            : _sessionValue(session, 'board', '');
        return typeof board === 'string' && board.length > 0 && board.indexOf('_') === -1;
    }

    // The ending in one line, from the session's own record.
    function _endLine(session) {
        var outcome = _outcome(session);
        var them = _firstName(session.contact_hash);
        var appId = _appId(session);
        var reason = _sessionValue(session, 'terminal_reason', '');
        var moves = parseInt(_sessionValue(session, 'move_count', 0), 10) || 0;

        if (outcome === 'ended') return 'The game is over.';
        if (_isResignation(session)) return outcome === 'won' ? them + ' resigned.' : 'You resigned.';
        if (appId === 'chess') {
            var full = parseInt(String(_sessionValue(session, 'fen', '')).split(' ')[5], 10) || 0;
            if (outcome === 'draw') {
                return {
                    sm: 'Stalemate. Neither side can move without checkmate.',
                    ins: 'Neither side has enough pieces left to checkmate.',
                    '3fr': 'The same position came up three times.',
                    '50m': 'Fifty moves passed without a capture or a pawn move.',
                    agr: 'You both agreed to a draw.'
                }[reason] || 'The game ended in a draw.';
            }
            if (reason === 'cm') {
                return outcome === 'won'
                    ? 'Checkmate against ' + them + (full ? ', in ' + _numberWord(full) + ' moves' : '') + '.'
                    : them + ' won by checkmate.';
            }
            return outcome === 'won' ? 'You beat ' + them + '.' : them + ' won the game.';
        }
        var line = appId === 'four_in_a_row' ? 'Four in a row' : 'Three in a row';
        if (outcome === 'draw') {
            return _boardFull(session)
                ? 'The board filled up without ' + line.toLowerCase() + '.'
                : 'You both agreed to a draw.';
        }
        if (outcome === 'won') {
            return line + ' against ' + them + (moves ? ', in ' + _numberWord(moves) + ' moves' : '') + '.';
        }
        var diagonal = false;
        if (appId === 'ttt') {
            var cells = _findWinCells(session.state || '');
            diagonal = !!cells && cells[1] === 4 && (cells[0] === 0 || cells[0] === 2);
        }
        return them + ' got ' + line.toLowerCase() + (diagonal ? ' on the diagonal' : '') + '.';
    }

    function _resultParts(session) {
        var outcome = _outcome(session);
        var them = _firstName(session.contact_hash);
        return {
            outcome: outcome,
            title: outcome === 'won' ? 'You won' : outcome === 'lost' ? them + ' won' : outcome === 'draw' ? 'Draw' : 'Game over',
            body: _endLine(session),
            tile: outcome === 'won' ? 'ui-tile--online' : 'ui-tile--neutral',
            glyph: outcome === 'won' ? _icon('trophy') : _icon('games'),
        };
    }

    function _resultActions(cls, quietPair) {
        return '<div class="' + cls + '">' +
            '<button type="button" class="ui-btn ui-btn--quiet' + (quietPair ? ' ui-btn--pair' : '') + '" data-game-action="remove">Remove game</button>' +
            '<button type="button" class="ui-btn ui-btn--primary' + (quietPair ? ' ui-btn--pair' : '') + '" data-game-action="rematch">Rematch</button></div>';
    }

    // Desktop: the card over the dimmed board. A phone shows the same result in a sheet.
    function _resultCard(session, isNew) {
        var r = _resultParts(session);
        return '<section class="game-result ui-desk-only' + (isNew ? ' is-new' : '') + '" role="dialog" aria-labelledby="game-result-title" aria-describedby="game-result-body">' +
            '<button type="button" class="ui-icon-btn ui-icon-btn--sm game-result-close" aria-label="Show the board" title="Show the board">' + _icon('x') + '</button>' +
            '<span class="ui-tile ui-tile--lg ' + r.tile + '">' + r.glyph + '</span>' +
            '<h3 class="game-result-title" id="game-result-title">' + escapeHtml(r.title) + '</h3>' +
            '<p class="game-result-body" id="game-result-body">' + escapeHtml(r.body) + '</p>' +
            _resultActions('game-result-actions', true) + '</section>';
    }

    // Under the board once the card is dismissed, and for a finished game opened later.
    function _afterControls(session, hidden, withStatus) {
        var outcome = _outcome(session);
        var dot = outcome === 'won' ? ' ui-dot--online' : '';
        var label = session.status === 'completed' ? _resultParts(session).title : '';
        if (session.status === 'completed' && _isResignation(session)) label = _statusText(session);
        var reason = _appId(session) === 'chess' && !_isResignation(session)
            ? CHESS_REASONS[_sessionValue(session, 'terminal_reason', '')] : '';
        if (label && reason) label += ' \u00b7 ' + reason;
        return '<div class="game-controls game-after"' + (hidden ? ' hidden' : '') + '>' +
            (withStatus !== false && label ? '<span class="ui-status"><span class="ui-dot' + dot + '"></span>' + escapeHtml(label) + '</span>' : '') +
            '<button type="button" class="ui-btn ui-btn--quiet ui-btn--pair" data-game-action="remove">Remove game</button>' +
            '<button type="button" class="ui-btn ui-btn--primary ui-btn--pair" data-game-action="rematch">' +
                (session.status === 'completed' ? 'Rematch' : 'Challenge again') + '</button></div>';
    }

    function _renderPlay(session, gameView, panel) {
        var appId = _appId(session);
        var context = _gameViewContext(session, panel);
        var meta = gameView.playerMeta ? gameView.playerMeta(session, context) || {} : {};
        var turn = _sessionValue(session, 'turn', '');
        var active = session.status === 'active';
        var myMove = active && !!_isMe(session, turn);
        var theirMove = active && !!turn && !myMove;
        var delivery = !active || session.delivery_state === 'failed' ? ''
            : session.delivery_state === 'propagated' ? 'Move stored in Offline Inbox'
            : _activeMoveDeliveryText(session.delivery_state);
        var veiled = session.status === 'completed' && !!_freshEndings[session.game_id];
        var isNew = veiled && !_resultShown[session.game_id];
        var myHash = _getMyHash(session);

        var html = '<div class="game-body">';
        if (active && session.delivery_state === 'failed') {
            html += _deliveryNotice(session, 'Your last move wasn’t delivered.');
        }
        html += '<div class="game-fit"><div class="game-play"><div class="game-board game-board--' + escapeHtml(appId || 'unknown') + '">' +
            _playerRow(session.contact_hash, _contactName(session.contact_hash), meta.theirs || '', theirMove ? 'Their move' : '') +
            '<div class="game-stage' + (veiled ? ' is-veiled' : '') + '">' +
                gameView.renderBoard(session, context) +
                (veiled ? '<div class="game-veil" aria-hidden="true"></div>' + _resultCard(session, isNew) : '') +
            '</div>' +
            _playerRow(myHash, 'You', [meta.mine, delivery].filter(Boolean).join(' · '), myMove ? 'Your move' : '', true) +
            '</div>';

        // Controls travel with the board, so the pair stays centred together.
        if (session.status === 'completed') {
            html += _afterControls(session, veiled);
        } else if (active) {
            var view = _gameView(appId);
            var controls = view && view.renderActiveControls
                ? view.renderActiveControls(session)
                : _renderStandardActiveControls(session, '');
            if (controls) html += '<div class="game-controls">' + controls + '</div>';
        }
        return html + '</div></div></div>';
    }

    function renderDetail() {
        var panel = document.getElementById('games-detail');
        if (!panel) return;

        var session = _getSelectedSession();
        if (!session) {
            panel.removeAttribute('data-game');
            panel.classList.remove('is-game');
            panel.innerHTML = _renderLanding(_allSessions.length === 0, false);
            _bindNewGameTriggers(panel);
            return;
        }

        var appId = _appId(session);
        var gameView = _gameView(appId);
        panel.setAttribute('data-game', appId);
        panel.classList.add('is-game');

        var html;
        if (session.status === 'pending' || session.status === 'declined' || session.status === 'expired') {
            var statusHtml = '';
            if (session.status === 'pending') {
                statusHtml = _isMe(session, session.challenger)
                    ? '<span class="ui-status ui-desk-only"><span class="ui-dot ui-dot--warning"></span>Waiting</span>'
                    : '<span class="ui-status ui-desk-only"><span class="ui-unread" aria-hidden="true"></span>Needs you</span>';
            }
            html = _detailHeader(session, statusHtml) + '<div class="game-body">' + _renderInvite(session) + '</div>';
        } else if (gameView) {
            html = _detailHeader(session, '') + _renderPlay(session, gameView, panel);
        } else {
            html = _detailHeader(session, '') + '<div class="game-body"><div class="ui-empty ui-empty--center">' +
                '<h3 class="ui-empty-title">This game needs a newer Ratspeak</h3>' +
                '<p class="ui-empty-body">Update Ratspeak to play ' + escapeHtml(_gameName(appId)) + '.</p></div></div>';
        }
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
                        var msg = (err && err.message) || 'Could not resend the game action';
                        showToast(msg, 'toast-error', 4000);
                    }
                });
            });
        }

        _bindResultEvents(session, panel);
        _bindControlEvents(session);
        if (gameView && (session.status === 'active' || session.status === 'completed')) {
            gameView.bindBoard(session, _gameViewContext(session, panel));
        }
        _presentEnding(session);
    }

    function _bindResultEvents(session, root) {
        var actions = root.querySelectorAll('[data-game-action]');
        for (var i = 0; i < actions.length; i++) {
            actions[i].addEventListener('click', function() {
                var action = this.getAttribute('data-game-action');
                if (action === 'remove') {
                    _confirmDeleteSession(session.game_id);
                } else if (action === 'rematch') {
                    _closeResultSheet();
                    startNewGame(_appId(session) || 'ttt', session.contact_hash);
                }
            });
        }
        var reveal = root.querySelectorAll('.game-veil, .game-result-close');
        for (var j = 0; j < reveal.length; j++) {
            reveal[j].addEventListener('click', function() { _revealBoard(session.game_id); });
        }
    }

    // Dismissing the result leaves the final board with the result line under it.
    function _revealBoard(sessionId) {
        if (!_freshEndings[sessionId]) return;
        delete _freshEndings[sessionId];
        _closeResultSheet();
        if (sessionId === _selectedSessionId) renderDetail();
    }

    function _presentEnding(session) {
        if (session.status !== 'completed' || !_freshEndings[session.game_id]) return;
        if (!_isViewingSession(session.game_id)) return;
        _resultShown[session.game_id] = true;
        if (_isPhoneWidth() && (!_resultSheet || _resultSheet.sessionId !== session.game_id)) {
            _openResultSheet(session);
        }
        _maybeCelebrateWin(session);
    }

    function _isPhoneWidth() {
        return !!(window.matchMedia && window.matchMedia('(max-width: 768px)').matches);
    }

    // The kit sheet on a phone: grab handle, full-width Rematch and Remove game, no small close.
    function _openResultSheet(session) {
        _closeResultSheet();
        var r = _resultParts(session);
        var sheet = _presentModal({
            className: 'game-result-modal',
            html: '<div class="ui-dialog" role="dialog" aria-modal="true" aria-labelledby="game-sheet-title" aria-describedby="game-sheet-body">' +
                '<div class="ui-dialog-handle"></div>' +
                '<div class="ui-dialog-body"><div class="game-result-sheet">' +
                    '<span class="ui-tile ui-tile--lg ' + r.tile + '">' + r.glyph + '</span>' +
                    '<h2 class="game-result-title" id="game-sheet-title">' + escapeHtml(r.title) + '</h2>' +
                    '<p class="game-result-body" id="game-sheet-body">' + escapeHtml(r.body) + '</p></div></div>' +
                _resultActions('ui-dialog-foot', false) + '</div>',
            onClose: function() {
                if (_resultSheet && _resultSheet.sessionId === session.game_id) _resultSheet = null;
                _revealBoard(session.game_id);
            },
        });
        sheet.sessionId = session.game_id;
        _resultSheet = sheet;
        _bindResultEvents(session, sheet.modal);
    }

    function _closeResultSheet() {
        var sheet = _resultSheet;
        _resultSheet = null;
        if (sheet) sheet.close();
    }

    // A kit dialog (a bottom sheet on a phone). Esc, the scrim, native Back,
    // and a downward drag on touch all close it.
    function _presentModal(opts) {
        var modal = document.createElement('div');
        modal.className = 'ui-modal games-modal' + (opts.className ? ' ' + opts.className : '');
        modal.innerHTML = opts.html;
        var dialog = modal.querySelector('.ui-dialog');
        dialog.classList.add('rs-sheet-shell', 'open');
        var previousFocus = document.activeElement;
        var closed = false;

        function onKey(e) {
            if (!modal.isConnected) return;
            // A dialog stacked above this one (a confirm) owns the keyboard.
            var active = document.activeElement;
            if (active && active !== document.body && !modal.contains(active)) return;
            if (e.key === 'Escape') {
                e.preventDefault();
                e.stopPropagation();
                close();
                return;
            }
            if (e.key !== 'Tab') return;
            var focusable = dialog.querySelectorAll('button:not([disabled]), input:not([disabled]), [tabindex="0"]');
            if (!focusable.length) return;
            var first = focusable[0];
            var last = focusable[focusable.length - 1];
            if (e.shiftKey && document.activeElement === first) {
                e.preventDefault();
                last.focus();
            } else if (!e.shiftKey && document.activeElement === last) {
                e.preventDefault();
                first.focus();
            } else if (!dialog.contains(document.activeElement)) {
                e.preventDefault();
                first.focus();
            }
        }

        function close(done) {
            if (closed) return;
            closed = true;
            document.removeEventListener('keydown', onKey, true);
            dialog.classList.remove('open');
            modal.classList.add('is-closing');
            setTimeout(function() {
                if (modal.parentNode) modal.remove();
                if (typeof done === 'function') {
                    done();
                } else if (previousFocus && previousFocus.isConnected && previousFocus.focus &&
                        (!(RS.ui && typeof RS.ui.prefersKeyboardFocus === 'function') || RS.ui.prefersKeyboardFocus())) {
                    previousFocus.focus();
                }
            }, 200);
            if (typeof opts.onClose === 'function') opts.onClose();
        }

        dialog._ratspeakDismiss = function() {
            if (opts.canClose && !opts.canClose()) return true;
            close();
            return true;
        };
        modal.addEventListener('click', function(e) {
            if (e.target === modal && (!opts.canClose || opts.canClose())) close();
        });
        document.addEventListener('keydown', onKey, true);
        document.body.appendChild(modal);
        if (dialog) {
            dialog.setAttribute('tabindex', '-1');
            requestAnimationFrame(function() {
                if (!dialog.contains(document.activeElement)) dialog.focus({ preventScroll: true });
            });
        }

        if (dialog && typeof isMobile === 'function' && isMobile() && RS.gestures &&
                typeof RS.gestures.attachDragDismiss === 'function') {
            RS.gestures.attachDragDismiss(dialog, {
                axis: 'y',
                blockIfScrolled: true,
                scrollContainerSelector: '.ui-dialog-body',
                skipIf: function(e) {
                    if (opts.canClose && !opts.canClose()) return true;
                    return !!(e.target.closest('button, input, label') && !e.target.closest('.ui-dialog-handle'));
                },
                onCommit: function() { close(); }
            });
        }
        return { modal: modal, dialog: dialog, close: close };
    }

    function _tttMyMarker(session) {
        var myMarker = session.my_marker || '';
        if (myMarker === 'X' || myMarker === 'O') return myMarker;
        return (_isMe(session, session.challenger) || _isMe(session, session.first_turn)) ? 'X' : 'O';
    }

    function _renderTTTBoard(session) {
        var board = session.state || '_________';
        var status = session.status;
        var isMyTurn = (status === 'active') && _isMe(session, session.turn);
        var winCells = _findWinCells(board);
        var iAmX = _tttMyMarker(session) === 'X';

        var markerClass = isMyTurn ? (iAmX ? ' my-marker-x' : ' my-marker-o') : '';
        var html = '<div class="ttt-grid' + (isMyTurn ? ' your-turn' : '') + markerClass + '" role="grid" aria-label="Tic-Tac-Toe board">';
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
        if (winCells && status === 'completed') {
            html += _renderWinLine(winCells);
        }
        return html + '</div>';
    }

    function _renderWinLine(cells) {
        var coords = cells.map(function(c) {
            return { x: (c % 3) * 33.33 + 16.67, y: Math.floor(c / 3) * 33.33 + 16.67 };
        });
        return '<svg class="ttt-win-line" viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">' +
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

    function _tttPlayerMeta(session) {
        var mine = _tttMyMarker(session);
        return { mine: 'Plays ' + mine, theirs: 'Plays ' + (mine === 'X' ? 'O' : 'X') };
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
        if (session.status === 'completed') _noteEnding(session);

        renderSessionList();
        renderDetail();

        if (!winCells && typeof haptic === 'function') {
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

    function _bindTTTCellEvents(session, context) {
        if (session.status !== 'active' || !_isMe(session, session.turn)) return;

        var root = (context && context.root) || document;
        var cells = root.querySelectorAll('.ttt-cell.clickable');
        for (var i = 0; i < cells.length; i++) {
            cells[i].addEventListener('click', (function(idx) {
                return function() {
                    _handleTTTMove(session, idx);
                };
            })(parseInt(cells[i].getAttribute('data-cell-index'))));
        }
    }

    // Chess
    var _chessSelected = {}; // { [session_id]: "e2" | null }

    function _chessMyColor(session) {
        return session.my_color || (session.metadata && session.metadata.my_color) || '';
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

    // Material each side has taken: value of the opponent's missing pieces.
    function _chessMaterial(pieces) {
        var values = { p: 1, n: 3, b: 3, r: 5, q: 9 };
        var starting = { p: 8, n: 2, b: 2, r: 2, q: 1 };
        var live = { w: { p: 0, n: 0, b: 0, r: 0, q: 0 }, b: { p: 0, n: 0, b: 0, r: 0, q: 0 } };
        Object.keys(pieces).forEach(function(sq) {
            var code = pieces[sq];
            if (live[code[0]] && live[code[0]].hasOwnProperty(code[1])) live[code[0]][code[1]] += 1;
        });
        var taken = { w: 0, b: 0 };
        Object.keys(values).forEach(function(k) {
            taken.w += Math.max(0, starting[k] - live.b[k]) * values[k];
            taken.b += Math.max(0, starting[k] - live.w[k]) * values[k];
        });
        return taken;
    }

    // "Plays white", plus "up 2" for the side ahead on material and "in check" for the side to move.
    function _chessPlayerMeta(session) {
        var myColor = _chessMyColor(session) === 'b' ? 'b' : 'w';
        var theirColor = myColor === 'w' ? 'b' : 'w';
        var fen = session.fen || (session.metadata && session.metadata.fen) || '';
        var taken = _chessMaterial(_chessFenToPieces(fen));
        var inCheck = !!(session.in_check || (session.metadata && session.metadata.in_check));
        var toMove = (String(fen).split(' ')[1] || 'w');
        function describe(color) {
            var words = ['Plays ' + (color === 'w' ? 'white' : 'black')];
            var lead = taken[color] - taken[color === 'w' ? 'b' : 'w'];
            if (lead > 0) words.push('up ' + lead);
            if (inCheck && session.status === 'active' && toMove === color) words.push('in check');
            return words.join(' · ');
        }
        return { mine: describe(myColor), theirs: describe(theirColor) };
    }

    function _renderChessBoard(session) {
        var fen = session.fen || (session.metadata && session.metadata.fen) || 'rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1';
        var pieces = _chessFenToPieces(fen);
        var myColor = _chessMyColor(session);
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

        var html = '<div class="chess-board' + (isMyTurn ? ' your-turn' : '') + '" role="grid" aria-label="Chess board" data-orient="' + orient + '">';
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
        return html + '</div>';
    }

    function _chessPieceName(piece) {
        if (!piece || piece.length < 2) return 'empty';
        var color = piece[0] === 'w' ? 'white' : 'black';
        var names = { p: 'pawn', n: 'knight', b: 'bishop', r: 'rook', q: 'queen', k: 'king' };
        return color + ' ' + (names[piece[1]] || 'piece');
    }

    function _bindChessSquareEvents(session, context) {
        if ((session.app_id || session.game) !== 'chess') return;
        if (session.status !== 'active' || !_isMe(session, session.turn)) return;

        var root = (context && context.root) || document;
        var squares = root.querySelectorAll('.chess-square.clickable');
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
                html += '<span class="game-controls-note">' + escapeHtml(_firstName(session.contact_hash)) + ' offers a draw</span>';
                html += '<button type="button" class="ui-btn ui-btn--quiet ui-btn--pair" id="games-draw-decline-btn">Decline draw</button>';
                html += '<button type="button" class="ui-btn ui-btn--primary ui-btn--pair" id="games-draw-accept-btn">Accept draw</button>';
            } else {
                html += '<span class="game-controls-note">Draw offer sent</span>';
            }
        } else if (_gameSupportsAction(session, 'draw_offer')) {
            html += '<button type="button" class="ui-btn ui-btn--pair" id="games-draw-offer-btn">' +
                escapeHtml(drawOfferLabel || 'Offer draw') + '</button>';
        }
        if (_gameSupportsAction(session, 'resign')) {
            html += '<button type="button" class="ui-btn ui-btn--danger ui-btn--pair" id="games-resign-btn">Resign</button>';
        }
        return html;
    }

    function _chessRenderActiveControls(session) {
        var drawLabel = '';
        if (session.draw_offer_reason === '3fr') drawLabel = 'Claim threefold repetition';
        if (session.draw_offer_reason === '50m') drawLabel = 'Claim the fifty-move rule';
        return _renderStandardActiveControls(session, drawLabel);
    }

    function _bindControlEvents(session) {
        _bindBtn('games-accept-btn', function() {
            var btn = document.getElementById('games-accept-btn');
            if (btn && btn.disabled) return;
            _setBusy(btn, 'Accepting…');
            _sendAction(session, 'accept');
        });
        _bindBtn('games-decline-btn', function() {
            var btn = document.getElementById('games-decline-btn');
            if (btn && btn.disabled) return;
            _setBusy(btn, 'Declining…');
            _sendAction(session, 'decline');
        });
        _bindBtn('games-cancel-btn', function() {
            var btn = document.getElementById('games-cancel-btn');
            var doCancel = function() {
                _setBusy(btn, 'Cancelling…');
                _sendAction(session, 'decline');
            };
            if (typeof rsConfirm === 'function') {
                rsConfirm({
                    title: 'Cancel this challenge?',
                    message: _contactName(session.contact_hash) +
                        ' is told the challenge was cancelled. You can send a new one any time.',
                    confirmText: 'Cancel challenge',
                    cancelText: 'Keep waiting',
                    danger: true,
                }).then(function(ok) { if (ok) doCancel(); });
            } else if (typeof showToast === 'function') {
                showToast('Confirmation dialog unavailable', 'toast-error', 3000);
            }
        });
        _bindBtn('games-resign-btn', function() {
            if (typeof rsConfirm === 'function') {
                rsConfirm({
                    title: 'Resign this game?',
                    message: _contactName(session.contact_hash) +
                        ' wins, and the game moves to Done. You can ask for a rematch afterwards.',
                    confirmText: 'Resign',
                    cancelText: 'Keep playing',
                    danger: true,
                }).then(function(ok) {
                    if (ok) _sendAction(session, 'resign');
                });
            } else if (typeof showToast === 'function') {
                showToast('Confirmation dialog unavailable', 'toast-error', 3000);
            }
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

    // Loading: a spinner first, and the label says what is happening.
    function _setBusy(btn, label) {
        if (!btn) return;
        btn.disabled = true;
        btn.setAttribute('aria-busy', 'true');
        btn.innerHTML = '<span class="ui-spin"></span>' + escapeHtml(label);
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
                showToast(_reasonToMessage('send_failed', action), 'toast-error', 4000);
            }
            if (sessionId === _selectedSessionId) renderDetail();
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
            case 'resend_required':       return 'Action saved on this device. Select Resend to try again';
            case 'lxmf_not_initialized': return 'Messaging not ready — wait a moment';
            case 'pack_failed':          return 'Action rejected — invalid envelope';
            case 'send_failed':
            default:
                return command === 'move'
                    ? 'Move couldn’t be delivered. Select Resend to try again'
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
                showToast(_reasonToMessage(reason, data.command), 'toast-error', 5000);
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
                if (session.status !== 'completed') {
                    delete _freshEndings[sid];
                    delete _resultShown[sid];
                }
                break;
            }
            delete _optimisticBackup[sid];
            renderSessionList();
            if (sid === _selectedSessionId) renderDetail();
        }

        if (typeof showToast === 'function') {
            showToast(_reasonToMessage(reason, data.command), 'toast-error', 4000);
        }
        if (typeof haptic === 'function') haptic('error');
    }

    // Supported games in gallery order: Chess, Four in a Row, Tic-Tac-Toe.
    function _newGameManifests() {
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
        return manifests.slice().sort(function(a, b) {
            var ia = GAME_ORDER.indexOf(a.app_id), ib = GAME_ORDER.indexOf(b.app_id);
            if (ia === -1) ia = GAME_ORDER.length;
            if (ib === -1) ib = GAME_ORDER.length;
            return (ia - ib) || (a.display_name || a.app_id).localeCompare(b.display_name || b.app_id);
        });
    }

    // A contact whose announces we have heard without Ratspeak's service can't play.
    function _canPlay(contact) {
        var services = Array.isArray(contact.services) ? contact.services : [];
        if (contact.supports_ratspeak === true || services.indexOf('ratspeak.client') !== -1) return true;
        return services.length === 0;
    }

    // Recent opponents first, then contacts by name with those who can't play last.
    function _newGamePeople(query) {
        var contacts = _getContacts().filter(function(c) { return c && c.hash; });
        var byHash = {};
        contacts.forEach(function(c) { byHash[c.hash] = c; });
        var recent = [];
        var seen = {};
        _allSessions.slice().sort(function(a, b) {
            return _sessionTime(b) - _sessionTime(a);
        }).forEach(function(s) {
            if (!s.contact_hash || seen[s.contact_hash] || recent.length >= 4) return;
            seen[s.contact_hash] = true;
            recent.push(byHash[s.contact_hash] || { hash: s.contact_hash, display_name: _contactName(s.contact_hash) });
        });
        var rest = contacts.filter(function(c) { return !seen[c.hash]; }).sort(function(a, b) {
            return ((_canPlay(a) ? 0 : 1) - (_canPlay(b) ? 0 : 1)) ||
                (a.display_name || '').localeCompare(b.display_name || '');
        });
        var q = String(query || '').trim().toLowerCase();
        function matches(c) {
            if (!q) return true;
            var name = (c.display_name || _contactName(c.hash) || '').toLowerCase();
            return name.indexOf(q) !== -1 || String(c.hash).toLowerCase().indexOf(q) === 0;
        }
        return { recent: recent.filter(matches), rest: rest.filter(matches), total: contacts.length + recent.length };
    }

    function showNewGameDialog(opts) {
        opts = opts && typeof opts === 'object' && !opts.target ? opts : {};
        if (typeof haptic === 'function') haptic('selection');
        if (document.querySelector('.ng-modal')) return;

        var manifests = _newGameManifests();
        var byId = {};
        manifests.forEach(function(m) { byId[m.app_id] = m; });
        var state = {
            game: opts.game && byId[opts.game] ? opts.game : null,
            opponent: opts.opponent || null,
            query: '',
            sending: false,
        };

        function gameName(appId) {
            return byId[appId] ? (byId[appId].display_name || appId) : _gameName(appId);
        }

        function tile(m) {
            var appId = escapeHtml(m.app_id);
            var desc = GAME_DESCRIPTIONS[m.app_id] || '';
            return '<label class="game-choice"><input class="game-choice-input" type="radio" name="ng-game" value="' + appId + '"' +
                ' aria-labelledby="ng-tn-' + appId + '"' + (desc ? ' aria-describedby="ng-td-' + appId + '"' : '') + '>' +
                '<span class="game-art" aria-hidden="true">' + (RS.games.art ? RS.games.art.preview(m.app_id, 'wide') : '') + '</span>' +
                '<span class="game-choice-copy"><span class="game-choice-top"><span class="game-choice-name" id="ng-tn-' + appId + '">' +
                    escapeHtml(m.display_name || m.app_id) + '</span><span class="game-check" aria-hidden="true">' + _icon('check') + '</span></span>' +
                    (desc ? '<span class="game-choice-desc" id="ng-td-' + appId + '">' + escapeHtml(desc) + '</span>' : '') +
                '</span></label>';
        }

        function pick(m) {
            var appId = escapeHtml(m.app_id);
            var desc = GAME_DESCRIPTIONS[m.app_id] || '';
            return '<button type="button" class="game-choice" data-pick="' + appId + '"' + (desc ? ' aria-describedby="ng-pd-' + appId + '"' : '') + '>' +
                '<span class="game-art" aria-hidden="true">' + (RS.games.art ? RS.games.art.preview(m.app_id, 'wide') : '') + '</span>' +
                '<span class="game-choice-copy"><span class="game-choice-top"><span class="game-choice-name">' + escapeHtml(m.display_name || m.app_id) +
                    '</span>' + _icon('chevron-right') + '</span>' +
                    (desc ? '<span class="game-choice-desc" id="ng-pd-' + appId + '">' + escapeHtml(desc) + '</span>' : '') +
                '</span></button>';
        }

        function person(c) {
            var hash = escapeHtml(c.hash);
            var playable = _canPlay(c);
            var heard = c.last_seen || _lastHeard(c.hash);
            var sub = !playable ? 'Games need Ratspeak'
                : (heard ? 'Heard ' + RS.kit.time.sentence(heard) : 'Not heard yet');
            var name = c.display_name || _contactName(c.hash) || 'Anonymous';
            return '<label class="ui-row ng-person' + (playable ? '' : ' is-off') + '"><input class="game-choice-input" type="radio" name="ng-opponent" value="' + hash + '"' +
                (state.opponent === c.hash ? ' checked' : '') + (playable ? '' : ' disabled') +
                ' aria-labelledby="ng-pn-' + hash + '" aria-describedby="ng-pm-' + hash + '">' + _avatar(c.hash, 32) +
                '<span class="ui-row-main"><span class="ui-row-title" id="ng-pn-' + hash + '">' + ratspeakDisplayNameHtml(name, c) + '</span>' +
                '<span class="ui-row-sub"><span class="t" id="ng-pm-' + hash + '">' + escapeHtml(sub) + '</span></span></span>' +
                (playable ? '<span class="game-check" aria-hidden="true">' + _icon('check') + '</span>' : '') + '</label>';
        }

        function peopleHtml() {
            if (_getContacts().length === 0 && _allSessions.length === 0) {
                return '<div class="ui-empty"><span class="ui-tile ui-tile--lg">' + _icon('user-plus') + '</span>' +
                    '<h3 class="ui-empty-title">No contacts yet</h3><p class="ui-empty-body">Games are played with contacts. Add someone first.</p>' +
                    '<div class="ui-empty-actions"><button type="button" class="ui-btn" id="ng-open-contacts">Open Contacts</button></div></div>';
            }
            var people = _newGamePeople(state.query);
            if (!people.recent.length && !people.rest.length) {
                return '<div class="ui-empty"><h3 class="ui-empty-title">No contacts match “' + escapeHtml(state.query.trim()) + '”</h3>' +
                    '<p class="ui-empty-body">Search checks names and the start of addresses.</p>' +
                    '<div class="ui-empty-actions"><button type="button" class="ui-link" data-clear-search>Clear search</button></div></div>';
            }
            var html = '';
            if (people.recent.length) html += '<p class="ui-caption">Recent</p>' + people.recent.map(person).join('');
            if (people.rest.length) html += '<p class="ui-caption">Contacts</p>' + people.rest.map(person).join('');
            return html;
        }

        var empty = _getContacts().length === 0 && _allSessions.length === 0;
        var sheet = _presentModal({
            className: 'ng-modal',
            canClose: function() { return !state.sending; },
            html: '<div class="ui-dialog ui-dialog--lg ng-dialog" role="dialog" aria-modal="true" aria-labelledby="ng-title">' +
                '<div class="ui-dialog-handle"></div>' +
                '<div class="ui-dialog-head">' +
                    '<button type="button" class="ui-icon-btn ng-back" aria-label="Back to games" title="Back">' + _icon('chevron-left') + '</button>' +
                    '<span class="ng-head-thumb"></span>' +
                    '<h2 class="ui-dialog-title" id="ng-title" tabindex="-1">New game</h2>' +
                    '<button type="button" class="ui-icon-btn ng-close" aria-label="Close" title="Close">' + _icon('x') + '</button>' +
                '</div>' +
                '<form class="ng-form" novalidate>' +
                    '<div class="ui-dialog-body ng-body">' +
                        '<div class="ng-picks">' + manifests.map(pick).join('') + '</div>' +
                        '<section class="ng-games" aria-labelledby="ng-gl"><h3 class="ui-section-title" id="ng-gl">Game</h3>' +
                            '<div class="ng-gallery" role="radiogroup" aria-labelledby="ng-gl">' + manifests.map(tile).join('') + '</div></section>' +
                        '<section class="ng-opp' + (empty ? ' is-empty' : '') + '" aria-labelledby="ng-ol"><div class="ng-people ui-scroll"><div class="ng-opp-head ui-scroll-head"><h3 class="ui-section-title" id="ng-ol">Opponent</h3>' +
                            (empty ? '' : '<div class="ui-search" role="search">' + _icon('search') +
                                '<input type="search" class="ng-search" placeholder="Search contacts" aria-label="Search contacts" autocorrect="off" autocapitalize="none" spellcheck="false">' +
                                '<button type="button" class="ui-icon-btn ui-icon-btn--sm ng-clear" aria-label="Clear search" title="Clear search" hidden>' + _icon('x') + '</button></div>') +
                            '</div><div class="ng-people-list" role="radiogroup" aria-labelledby="ng-ol"></div></div></section>' +
                    '</div>' +
                    '<div class="ui-dialog-foot"><p class="ng-hint" id="ng-hint" aria-live="polite"></p>' +
                        '<button type="button" class="ui-btn ui-btn--quiet ui-desk-only ng-cancel">Cancel</button>' +
                        '<button type="submit" class="ui-btn ui-btn--primary ng-send" aria-describedby="ng-hint" disabled></button></div>' +
                '</form></div>',
        });
        var root = sheet.dialog;
        var form = root.querySelector('.ng-form');
        var peopleEl = root.querySelector('.ng-people-list');
        var search = root.querySelector('.ng-search');
        var clear = root.querySelector('.ng-clear');

        function renderPeople() {
            peopleEl.innerHTML = peopleHtml();
            var open = peopleEl.querySelector('#ng-open-contacts');
            if (open) {
                open.addEventListener('click', function() {
                    sheet.close(function() {
                        if (typeof switchView === 'function') switchView('contacts', { pushState: true });
                    });
                });
            }
            var clearLink = peopleEl.querySelector('[data-clear-search]');
            if (clearLink) clearLink.addEventListener('click', function() { setQuery(''); if (search) search.focus(); });
        }

        function setQuery(q) {
            state.query = q;
            if (search && search.value !== q) search.value = q;
            if (clear) clear.hidden = !q;
            renderPeople();
        }

        function update() {
            var phone = _isPhoneWidth();
            var step = phone && !state.game ? 'pick' : 'who';
            root.classList.toggle('ng-step-pick', step === 'pick');
            root.classList.toggle('ng-step-who', step === 'who');
            var title = root.querySelector('#ng-title');
            title.textContent = phone && state.game ? gameName(state.game) : 'New game';
            root.querySelector('.ng-head-thumb').innerHTML = phone && state.game ? _thumb(state.game) : '';
            root.querySelectorAll('input[name="ng-game"]').forEach(function(input) {
                input.checked = input.value === state.game;
                input.disabled = state.sending;
            });
            root.querySelectorAll('input[name="ng-opponent"]').forEach(function(input) {
                input.checked = input.value === state.opponent;
                if (state.sending) input.disabled = true;
            });
            form.classList.toggle('is-sending', state.sending);
            if (search) search.disabled = state.sending;
            root.querySelectorAll('.ng-close, .ng-back, .ng-cancel').forEach(function(btn) { btn.disabled = state.sending; });

            var send = root.querySelector('.ng-send');
            var opponentName = state.opponent ? _contactName(state.opponent) : '';
            send.disabled = state.sending || !(state.game && state.opponent);
            send.setAttribute('aria-busy', state.sending ? 'true' : 'false');
            if (state.sending) {
                send.innerHTML = '<span class="ui-spin"></span>Sending challenge…';
            } else if (state.game && state.opponent) {
                send.innerHTML = '<span class="ng-send-label"><span class="f">Challenge </span><span class="n">' +
                    escapeHtml(opponentName) + '</span><span class="f"> to ' + escapeHtml(gameName(state.game)) + '</span></span>';
            } else {
                send.innerHTML = '<span class="ng-send-label">Send challenge</span>';
            }
            root.querySelector('#ng-hint').textContent = empty ? 'Add a contact to start a game.'
                : !state.game && !state.opponent ? 'Choose a game and an opponent.'
                : !state.game ? 'Choose a game.'
                : !state.opponent ? 'Choose an opponent.'
                : 'They have 24 hours to accept.';
        }

        renderPeople();
        update();

        form.addEventListener('change', function(e) {
            var input = e.target;
            if (!input || input.type !== 'radio') return;
            if (typeof haptic === 'function') haptic('selection');
            if (input.name === 'ng-game') state.game = input.value;
            if (input.name === 'ng-opponent') state.opponent = input.value;
            update();
        });
        root.querySelectorAll('[data-pick]').forEach(function(btn) {
            btn.addEventListener('click', function() {
                if (typeof haptic === 'function') haptic('selection');
                state.game = btn.getAttribute('data-pick');
                update();
                root.querySelector('#ng-title').focus();
            });
        });
        root.querySelector('.ng-back').addEventListener('click', function() {
            state.game = null;
            update();
        });
        root.querySelector('.ng-close').addEventListener('click', function() { sheet.close(); });
        root.querySelector('.ng-cancel').addEventListener('click', function() { sheet.close(); });
        if (search) {
            search.addEventListener('input', function() { setQuery(search.value); update(); });
            clear.addEventListener('click', function() { setQuery(''); update(); search.focus(); });
        }
        form.addEventListener('submit', function(e) {
            e.preventDefault();
            if (state.sending || !state.game || !state.opponent) return;
            if (typeof haptic === 'function') haptic('selection');
            state.sending = true;
            update();
            startNewGame(state.game, state.opponent).then(function(ok) {
                if (ok) {
                    state.sending = false;
                    sheet.close(function() {});
                    return;
                }
                state.sending = false;
                renderPeople();
                update();
            });
        });

        if (!(typeof isMobile === 'function' && isMobile())) {
            requestAnimationFrame(function() {
                var target = state.game && !state.opponent && search ? search
                    : root.querySelector('input[name="ng-game"]:checked') || root.querySelector('input[name="ng-game"]');
                if (target) target.focus();
            });
        }
    }

    // Resolves true once the challenge is on its way; failures keep the dialog open.
    function startNewGame(appId, contactHash) {
        var arr = new Uint8Array(8);
        crypto.getRandomValues(arr);
        var sessionId = '';
        for (var i = 0; i < arr.length; i++) {
            sessionId += ('0' + arr[i].toString(16)).slice(-2);
        }
        if (!_beginSessionAction(sessionId)) return Promise.resolve(false);

        return RS.invoke('send_game_action', {
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
                return false;
            }
            var selected = (ack && ack.session_id) ? ack.session_id : sessionId;
            RS.invoke('get_all_game_sessions').then(function(sessions) {
                if (Array.isArray(sessions)) {
                    _allSessions = sessions;
                    if (_findSession(selected)) {
                        selectSession(selected);
                    } else {
                        renderSessionList();
                        renderDetail();
                    }
                }
            }).catch(function() {});
            return true;
        }).catch(function() {
            _finishSessionAction(sessionId);
            if (typeof showToast === 'function') {
                showToast('Could not send the challenge', 'toast-error', 4000);
            }
            return false;
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
            renderDetail();
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
                showToast('Game action rejected: ' + message, 'toast-error', 5000);
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
        var appForeground = typeof RS === 'undefined' ||
            typeof RS.isAttentionForeground !== 'function' ||
            RS.isAttentionForeground();

        var view = _gameView(_appId(record));
        if (view && view.onSessionDelta) {
            view.onSessionDelta(record, prev, _gameViewContext(record));
        }

        var isNew = !prev;
        if (isNew && record.status === 'pending' && !_isMe(record, record.challenger)) {
            if (appForeground && typeof currentView !== 'undefined' && currentView !== 'games') {
                if (typeof showToast === 'function') showToast('Game challenge from ' + _contactName(record.contact_hash), 'toast-action', 5000, function() { window.openGameSession(record.game_id); });
                if (typeof haptic === 'function') haptic('success');
            }
            if (!window.__TAURI_INTERNALS__ && !appForeground && typeof rsNotify !== 'undefined') {
                rsNotify.send({
                    title: 'Game challenge',
                    body: _contactName(record.contact_hash) + ' challenged you to a game'
                });
            }
        }

        // Foreground updates use an in-app action when this board is not
        // visible. Background browser updates use only the OS fallback; Tauri
        // background notifications are owned by the runtime.
        var movedSinceLast = prev &&
            _sessionValue(record, 'move_count', null) !==
            _sessionValue(prev, 'move_count', null);
        var notViewingThisGame = !_isViewingSession(record.game_id);
        if (movedSinceLast && notViewingThisGame && record.status === 'active') {
            if (appForeground) {
                if (typeof showToast === 'function') showToast('Game update from ' + _contactName(record.contact_hash), 'toast-action', 3000, function() { window.openGameSession(record.game_id); });
                if (typeof haptic === 'function') haptic('light');
            }
            if (!window.__TAURI_INTERNALS__ && !appForeground && typeof rsNotify !== 'undefined') {
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

        if (prev && record.status === 'completed' && prevStatus !== 'completed') _noteEnding(record);
    }

    function updateGamesBadge() {
        var total = 0;
        for (var i = 0; i < _allSessions.length; i++) {
            if (_allSessions[i].unread > 0 && !_isViewingSession(_allSessions[i].game_id)) total++;
        }
        if (window.RS && RS.shell) RS.shell.setGamesWaiting(total);
    }

    // All, Active, Pending, Done: one segmented control, arrow keys move within it.
    function _initFilter() {
        var group = document.querySelector('#games-filter .ui-seg');
        if (!group) return;
        var options = group.querySelectorAll('[data-filter]');
        function choose(button, focus) {
            _activeFilter = FILTERS.indexOf(button.getAttribute('data-filter')) !== -1
                ? button.getAttribute('data-filter') : 'all';
            for (var j = 0; j < options.length; j++) {
                var on = options[j] === button;
                options[j].setAttribute('aria-checked', on ? 'true' : 'false');
                options[j].tabIndex = on ? 0 : -1;
            }
            if (focus) button.focus();
            renderSessionList();
        }
        for (var i = 0; i < options.length; i++) {
            options[i].addEventListener('click', function() { choose(this, false); });
            options[i].addEventListener('keydown', (function(index) {
                return function(e) {
                    var next = index;
                    if (e.key === 'ArrowRight' || e.key === 'ArrowDown') next = (index + 1) % options.length;
                    else if (e.key === 'ArrowLeft' || e.key === 'ArrowUp') next = (index - 1 + options.length) % options.length;
                    else return;
                    e.preventDefault();
                    choose(options[next], true);
                };
            })(i));
        }
    }

    function _initNewGameBtn() {
        _bindBtn('games-new-btn', function() { showNewGameDialog(); });
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
                renderDetail();
                updateGamesBadge();
            }
        }).catch(function() {});
    };

    // Deep-link entry point for notification taps (route lrgp:<session_id>).
    // Switches to the games view, refreshes sessions, then opens the board.
    window.openGameSession = function(sessionId, isCurrent) {
        isCurrent = typeof isCurrent === 'function' ? isCurrent : function() { return true; };
        if (!sessionId || !isCurrent()) return Promise.resolve(false);
        if (typeof switchView === 'function') switchView('games');
        function openExistingSession() {
            if (!isCurrent() || !_findSession(sessionId)) return false;
            selectSession(sessionId);
            return true;
        }
        return RS.invoke('get_all_game_sessions').then(function(sessions) {
            if (!isCurrent()) return false;
            if (Array.isArray(sessions)) {
                _allSessions = sessions;
                renderSessionList();
            }
            return openExistingSession();
        }).catch(function() { return openExistingSession(); });
    };

    window.updateGamesBadge = updateGamesBadge;

    window.gamesTabClear = function() {
        _allSessions = [];
        _selectedSessionId = null;
        _contactNameCache = {};
        _celebratedWins = {};
        _freshEndings = {};
        _resultShown = {};
        _closeResultSheet();
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
                boardSelector: '.ttt-grid',
                actions: ['challenge', 'accept', 'decline', 'move', 'resign',
                    'draw_offer', 'draw_accept', 'draw_decline'],
                renderBoard: _renderTTTBoard,
                bindBoard: _bindTTTCellEvents,
                playerMeta: _tttPlayerMeta,
                onSessionDelta: _tttSessionDelta,
                celebrationOptions: function() {
                    return { count: 48, duration: 1600 };
                },
            });
        }
        if (!RS.games.views.has('chess')) {
            RS.games.views.register('chess', {
                displayName: 'Chess',
                boardSelector: '.chess-board',
                actions: ['challenge', 'accept', 'decline', 'move', 'resign',
                    'draw_offer', 'draw_accept', 'draw_decline'],
                renderBoard: _renderChessBoard,
                bindBoard: _bindChessSquareEvents,
                playerMeta: _chessPlayerMeta,
                renderActiveControls: _chessRenderActiveControls,
                celebrationOptions: _chessCelebrationOptions,
                actionPayload: _chessActionPayload,
            });
        }
    }

    function _init() {
        _loadGameManifests();
        _initFilter();
        _initNewGameBtn();
        _initGameEvents();
        renderSessionList();
        renderDetail();
    }

    _registerBuiltinGameViews();

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', _init);
    } else {
        _init();
    }

})();
