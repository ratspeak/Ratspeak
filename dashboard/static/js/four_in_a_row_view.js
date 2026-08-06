(function(global) {
    'use strict';

    var RS = global.RS = global.RS || {};
    if (!RS.games || !RS.games.views) {
        throw new Error('Game view registry must load before four_in_a_row_view.js');
    }

    var APP_ID = 'four_in_a_row';
    var ROWS = 6;
    var COLUMNS = 7;
    var CELL_COUNT = ROWS * COLUMNS;
    var EMPTY_BOARD = '__________________________________________';
    var _settlingCells = Object.create(null);
    var _tracedWins = Object.create(null);

    function _meta(session, key, fallback) {
        return RS.games.state.value(session, key, fallback);
    }

    function _board(session) {
        var value = _meta(session, 'board', '');
        if (typeof value !== 'string' || value.length !== CELL_COUNT || /[^_AB]/.test(value)) {
            return EMPTY_BOARD;
        }
        return value;
    }

    function _escape(value) {
        return String(value == null ? '' : value)
            .replace(/&/g, '&amp;')
            .replace(/</g, '&lt;')
            .replace(/>/g, '&gt;')
            .replace(/"/g, '&quot;')
            .replace(/'/g, '&#39;');
    }

    function _markerName(marker) {
        return marker === 'B' ? 'Ring' : 'Node';
    }

    function _myMarker(session, context) {
        var marker = _meta(session, 'my_marker', '');
        if (marker === 'A' || marker === 'B') return marker;
        return context && context.isMe && context.isMe(_meta(session, 'first_turn', '')) ? 'A' : 'B';
    }

    function _landingRow(board, column) {
        for (var row = ROWS - 1; row >= 0; row--) {
            if (board[row * COLUMNS + column] === '_') return row;
        }
        return -1;
    }

    function _winningCells(board) {
        var directions = [[0, 1], [1, 0], [1, 1], [1, -1]];
        for (var row = 0; row < ROWS; row++) {
            for (var column = 0; column < COLUMNS; column++) {
                var marker = board[row * COLUMNS + column];
                if (marker === '_') continue;
                for (var d = 0; d < directions.length; d++) {
                    var dr = directions[d][0];
                    var dc = directions[d][1];
                    var endRow = row + dr * 3;
                    var endColumn = column + dc * 3;
                    if (endRow < 0 || endRow >= ROWS || endColumn < 0 || endColumn >= COLUMNS) {
                        continue;
                    }
                    var cells = [];
                    var won = true;
                    for (var step = 0; step < 4; step++) {
                        var index = (row + dr * step) * COLUMNS + column + dc * step;
                        cells.push(index);
                        if (board[index] !== marker) won = false;
                    }
                    if (won) {
                        return {
                            cells: cells,
                            start: { row: row, column: column },
                            end: { row: endRow, column: endColumn },
                        };
                    }
                }
            }
        }
        return null;
    }

    function _token(marker, extraClass) {
        if (marker !== 'A' && marker !== 'B') return '<span class="four-slot" aria-hidden="true"></span>';
        return '<span class="four-token four-token-' + marker.toLowerCase() +
            (extraClass ? ' ' + extraClass : '') + '" aria-hidden="true"></span>';
    }

    function _playerKey(label, marker, active) {
        return '<div class="four-player-key' + (active ? ' active-turn' : '') + '">' +
            _token(marker, 'four-token-key') +
            '<span class="four-player-label">' + _escape(label) + '</span>' +
            '<span class="four-player-marker">' + _markerName(marker) + '</span>' +
        '</div>';
    }

    function _winTrace(win, animate) {
        if (!win) return '';
        var x1 = win.start.column * 100 + 50;
        var y1 = win.start.row * 100 + 50;
        var x2 = win.end.column * 100 + 50;
        var y2 = win.end.row * 100 + 50;
        return '<svg class="four-win-trace' + (animate ? ' animate' : '') +
            '" viewBox="0 0 700 600" preserveAspectRatio="none" aria-hidden="true">' +
            '<line x1="' + x1 + '" y1="' + y1 + '" x2="' + x2 + '" y2="' + y2 + '"></line>' +
        '</svg>';
    }

    function _renderBoard(session, context) {
        var board = _board(session);
        var myMarker = _myMarker(session, context);
        var theirMarker = myMarker === 'A' ? 'B' : 'A';
        var isMyTurn = session.status === 'active' && context && context.isMe &&
            context.isMe(_meta(session, 'turn', ''));
        var myTurnActive = session.status === 'active' && isMyTurn;
        var theirTurnActive = session.status === 'active' && !isMyTurn && !!_meta(session, 'turn', '');
        var opponentName = context && context.contactName
            ? context.contactName(session.contact_hash)
            : 'Opponent';
        var win = _winningCells(board);
        var winning = win ? win.cells : [];
        var settling = _settlingCells[session.game_id];
        var settlingCell = settling && settling.until > Date.now() ? settling.cell : -1;
        if (settling) delete _settlingCells[session.game_id];
        if (!win && session.game_id) delete _tracedWins[session.game_id];
        var animateTrace = !!(win && session.game_id &&
            _meta(session, 'terminal', '') === 'win' && !_tracedWins[session.game_id]);
        if (animateTrace) _tracedWins[session.game_id] = true;
        var landingRows = [];
        for (var column = 0; column < COLUMNS; column++) landingRows[column] = _landingRow(board, column);

        var html = '<div class="four-board-wrap' + (isMyTurn ? ' your-turn' : '') + '">';
        html += _playerKey(opponentName, theirMarker, theirTurnActive);
        html += '<div class="four-board-stage">';
        html += '<div class="four-board my-marker-' + myMarker.toLowerCase() +
            '" role="grid" aria-label="Four in a Row board" aria-rowcount="6" aria-colcount="7">';
        for (var row = 0; row < ROWS; row++) {
            for (var col = 0; col < COLUMNS; col++) {
                var cellIndex = row * COLUMNS + col;
                var marker = board[cellIndex];
                var landing = landingRows[col];
                var classes = ['four-cell'];
                if (marker !== '_') classes.push('occupied', 'marker-' + marker.toLowerCase());
                if (row === landing) classes.push('four-landing-cell');
                if (winning.indexOf(cellIndex) !== -1) classes.push('winning');
                if (cellIndex === settlingCell) classes.push('just-settled');

                var cellLabel = 'Row ' + (row + 1) + ', column ' + (col + 1) + ': ';
                if (marker === '_') {
                    cellLabel += 'empty';
                } else {
                    cellLabel += _markerName(marker) + ' signal';
                }
                html += '<div role="gridcell" class="' + classes.join(' ') +
                    '" data-cell-index="' + cellIndex + '" data-column="' + col +
                    '" data-landing-row="' + landing + '" aria-rowindex="' + (row + 1) +
                    '" aria-colindex="' + (col + 1) + '" aria-label="' + _escape(cellLabel) + '">' +
                    _token(marker, '') +
                '</div>';
            }
        }
        html += _winTrace(win, animateTrace);
        html += '</div>';
        html += '<div class="four-lane-actions" role="group" aria-label="Column drop controls">';
        for (var lane = 0; lane < COLUMNS; lane++) {
            var laneLanding = landingRows[lane];
            var canDrop = isMyTurn && laneLanding >= 0;
            var actionLabel;
            if (laneLanding < 0) {
                actionLabel = 'Column ' + (lane + 1) + ' is full';
            } else if (canDrop) {
                actionLabel = 'Drop ' + _markerName(myMarker) + ' in column ' + (lane + 1) +
                    '; lands in row ' + (laneLanding + 1);
            } else {
                actionLabel = 'Column ' + (lane + 1) + '; wait for your turn';
            }
            html += '<button type="button" class="four-lane-action" data-column="' + lane +
                '" data-landing-row="' + laneLanding + '" aria-label="' + _escape(actionLabel) +
                '"' + (canDrop ? '' : ' disabled') + '></button>';
        }
        html += '</div>';
        html += '</div>';
        html += _playerKey('You', myMarker, myTurnActive);

        if (session.status === 'pending') {
            var initiator = session.challenger || session.initiator || _meta(session, 'initiator', '');
            var incoming = !(context && context.isMe && context.isMe(initiator));
            html += '<div class="four-board-overlay">' +
                (incoming ? 'Challenge received!' : 'Waiting for response…') +
            '</div>';
        }
        if (session.status === 'completed') {
            var terminal = _meta(session, 'terminal', '');
            var won = context && context.isMe && context.isMe(_meta(session, 'winner', ''));
            var result = terminal === 'draw'
                ? 'Draw'
                : (terminal === 'resign'
                    ? (won ? 'Opponent resigned' : 'You resigned')
                    : (won ? 'You won!' : 'You lost'));
            var resultClass = terminal === 'draw' ? 'draw' : (won ? 'won' : 'lost');
            html += '<div class="four-result ' + resultClass + '" role="status">' +
                _escape(result) + '</div>';
        }
        html += '</div>';
        return html;
    }

    function _clearPreview(root) {
        var previews = root.querySelectorAll('.four-cell.is-preview');
        for (var i = 0; i < previews.length; i++) previews[i].classList.remove('is-preview');
    }

    function _previewColumn(root, column) {
        _clearPreview(root);
        var landing = root.querySelector('.four-cell.four-landing-cell[data-column="' + column + '"]');
        if (landing) landing.classList.add('is-preview');
    }

    function _applyOptimisticMove(session, context, column, landingRow) {
        var board = _board(session).split('');
        var marker = _myMarker(session, context);
        var cell = landingRow * COLUMNS + column;
        if (landingRow < 0 || board[cell] !== '_') return;
        board[cell] = marker;
        session.board = board.join('');
        session.move_count = (parseInt(_meta(session, 'move_count', 0), 10) || 0) + 1;
        session.last_column = column;
        session.last_row = landingRow;
        session.last_cell = cell;
        session.turn = session.contact_hash;
        _settlingCells[session.game_id] = { cell: cell, until: Date.now() + 500 };
    }

    function _bindBoard(session, context) {
        if (!context || !context.root || !context.sendMove || session.status !== 'active' ||
                !context.isMe(_meta(session, 'turn', ''))) return;
        var root = context.root;
        var lanes = root.querySelectorAll('.four-lane-action:not(:disabled)');
        for (var i = 0; i < lanes.length; i++) {
            lanes[i].addEventListener('click', (function(lane) {
                return function() {
                    var column = parseInt(lane.getAttribute('data-column'), 10);
                    var landingRow = parseInt(lane.getAttribute('data-landing-row'), 10);
                    context.sendMove({ c: column }, {
                        fields: ['board', 'last_column', 'last_row', 'last_cell'],
                        apply: function(liveSession) {
                            _applyOptimisticMove(liveSession, context, column, landingRow);
                        },
                    });
                };
            })(lanes[i]));
            lanes[i].addEventListener('pointerenter', (function(lane) {
                return function() { _previewColumn(root, lane.getAttribute('data-column')); };
            })(lanes[i]));
            lanes[i].addEventListener('pointerleave', function() { _clearPreview(root); });
            lanes[i].addEventListener('focus', (function(lane) {
                return function() { _previewColumn(root, lane.getAttribute('data-column')); };
            })(lanes[i]));
            lanes[i].addEventListener('blur', function() { _clearPreview(root); });
            lanes[i].addEventListener('keydown', (function(index) {
                return function(event) {
                    var next = index;
                    if (event.key === 'ArrowLeft') next = Math.max(0, index - 1);
                    else if (event.key === 'ArrowRight') next = Math.min(lanes.length - 1, index + 1);
                    else if (event.key === 'Home') next = 0;
                    else if (event.key === 'End') next = lanes.length - 1;
                    else return;
                    event.preventDefault();
                    lanes[next].focus();
                };
            })(i));
        }
    }

    function _activeStatusText(session, context) {
        var myMarker = _myMarker(session, context);
        var theirMarker = myMarker === 'A' ? 'B' : 'A';
        if (context && context.isMe && context.isMe(_meta(session, 'turn', ''))) {
            return 'Your turn · ' + _markerName(myMarker);
        }
        if (_meta(session, 'turn', '')) {
            var name = context && context.contactName
                ? context.contactName(session.contact_hash)
                : 'Opponent';
            return name + '\u2019s turn · ' + _markerName(theirMarker);
        }
        return '';
    }

    function _detailChips(session, context) {
        var chips = ['You: ' + _markerName(_myMarker(session, context))];
        var lastColumn = parseInt(_meta(session, 'last_column', -1), 10);
        if (lastColumn >= 0 && lastColumn < COLUMNS) chips.push('Last: column ' + (lastColumn + 1));
        return chips;
    }

    function _onSessionDelta(record, previous) {
        if (!record || !record.game_id || !previous) return;
        var board = _board(record);
        if (board === _board(previous)) return;
        var cell = parseInt(_meta(record, 'last_cell', -1), 10);
        if (cell >= 0 && cell < CELL_COUNT) {
            _settlingCells[record.game_id] = { cell: cell, until: Date.now() + 500 };
        }
    }

    function _celebrationOptions() {
        var styles = global.getComputedStyle
            ? global.getComputedStyle(global.document.documentElement)
            : null;
        function color(name, fallback) {
            return styles ? (styles.getPropertyValue(name) || fallback).trim() : fallback;
        }
        return {
            count: 56,
            duration: 1700,
            colors: [
                color('--accent', '#D2693B'),
                color('--ble-accent-fg', '#3A839B'),
                color('--status-online-fg', '#2E8B57'),
                color('--text-secondary', '#A59D99'),
            ],
        };
    }

    if (!RS.games.views.has(APP_ID)) {
        RS.games.views.register(APP_ID, {
            displayName: 'Four in a Row',
            icon: '\u25CF\u25CF\u25CF\u25CF',
            themeClass: 'games-theme-four-in-a-row',
            boardSelector: '.four-board',
            actions: ['challenge', 'accept', 'decline', 'move', 'resign',
                'draw_offer', 'draw_accept', 'draw_decline'],
            renderBoard: _renderBoard,
            bindBoard: _bindBoard,
            activeStatusText: _activeStatusText,
            detailChips: _detailChips,
            onSessionDelta: _onSessionDelta,
            celebrationOptions: _celebrationOptions,
        });
    }
})(window);
