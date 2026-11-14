// Board art for game tiles, thumbnails, and the New game gallery: the live
// boards' tokens and the app's chess sprite, drawn as small SVGs.
(function(global) {
    'use strict';

    var RS = global.RS = global.RS || {};
    RS.games = RS.games || {};

    var SPRITE = '/static/assets/chess-pieces.svg#';
    var SAMPLE_FEN = 'r1bqkbnr/pppp1ppp/2n5/4p3/2B1P3/5N2/PPPP1PPP/RNBQK2R';
    var SAMPLE_FOUR = '______________' + '____A__' + '___AB__' + '__BBA__' + '_BAABA_';
    var SAMPLE_TTT = 'X_O_O_X_X';
    var FILES = 'abcdefgh';
    var _uid = 0;

    function fenPieces(fen) {
        var rows = String(fen || '').split(' ')[0].split('/');
        var out = {};
        if (rows.length !== 8) return out;
        for (var y = 0; y < 8; y++) {
            var x = 0;
            for (var i = 0; i < rows[y].length; i++) {
                var ch = rows[y][i];
                if (/\d/.test(ch)) { x += parseInt(ch, 10); continue; }
                out[x + ',' + y] = (ch === ch.toUpperCase() ? 'w' : 'b') + ch.toLowerCase();
                x++;
            }
        }
        return out;
    }

    // UCI "e2e4" -> [fromX, fromY, toX, toY] in board coordinates (white at the bottom).
    function uciSquares(uci) {
        if (!uci || uci.length < 4) return null;
        var fx = FILES.indexOf(uci[0]), tx = FILES.indexOf(uci[2]);
        var fy = 8 - parseInt(uci[1], 10), ty = 8 - parseInt(uci[3], 10);
        if (fx < 0 || tx < 0 || isNaN(fy) || isNaN(ty)) return null;
        return [fx, fy, tx, ty];
    }

    // view: [x0, y0, width, height] in squares.
    function chess(view, fen, last, extra) {
        var pcs = fenPieces(fen || SAMPLE_FEN);
        var x0 = view[0], y0 = view[1], w = view[2], h = view[3], p = 0.16;
        last = last || [-1, -1, -1, -1];
        var s = '<svg class="gp gp-chess' + (extra ? ' ' + extra : '') + '" viewBox="' + (x0 - p) + ' ' + (y0 - p) + ' ' +
            (w + 2 * p) + ' ' + (h + 2 * p) + '" aria-hidden="true" focusable="false">';
        s += '<rect class="gp-chess-frame" x="' + (x0 - p) + '" y="' + (y0 - p) + '" width="' + (w + 2 * p) +
            '" height="' + (h + 2 * p) + '" rx="0.24"/>';
        s += '<rect class="gp-l" x="' + x0 + '" y="' + y0 + '" width="' + w + '" height="' + h + '"/>';
        for (var y = y0; y < y0 + h; y++) {
            for (var x = x0; x < x0 + w; x++) {
                var light = (x + y) % 2 === 0;
                var isLast = (x === last[0] && y === last[1]) || (x === last[2] && y === last[3]);
                if (light && !isLast) continue;
                s += '<rect class="' + (light ? 'gp-l' : 'gp-d') + (isLast ? ' gp-last' : '') + '" x="' + x + '" y="' + y +
                    '" width="1.01" height="1.01"/>';
            }
        }
        Object.keys(pcs).forEach(function(key) {
            var xy = key.split(','), px = +xy[0], py = +xy[1];
            if (px < x0 || px >= x0 + w || py < y0 || py >= y0 + h) return;
            var mover = px === last[2] && py === last[3];
            s += '<use href="' + SPRITE + pcs[key] + '" x="' + px + '" y="' + py + '" width="1" height="1"' +
                (mover ? ' class="gp-mover" style="--gp-dx:' + (last[0] - px) + 'px;--gp-dy:' + (last[1] - py) + 'px"' : '') + '/>';
        });
        return s + '</svg>';
    }

    // board: 42 cells, rows top to bottom; A and B are the two sides.
    function four(board, lastCell, extra) {
        board = typeof board === 'string' && board.length === 42 ? board : SAMPLE_FOUR;
        var id = 'gph' + (++_uid), pad = 0.22, W = 7 + 2 * pad, H = 6 + 2 * pad;
        var s = '<svg class="gp gp-four' + (extra ? ' ' + extra : '') + '" viewBox="0 0 ' + W + ' ' + H +
            '" aria-hidden="true" focusable="false">';
        s += '<defs><pattern id="' + id + '" patternUnits="userSpaceOnUse" width="0.16" height="0.16" patternTransform="rotate(45)">' +
            '<rect class="gp-hatch" x="0" y="0" width="0.065" height="0.16"/></pattern></defs>';
        s += '<rect class="gp-four-frame" x="0.02" y="0.02" width="' + (W - 0.04) + '" height="' + (H - 0.04) + '" rx="0.42"/>';
        for (var c = 1; c < 7; c++) {
            s += '<line class="gp-four-lane" x1="' + (pad + c) + '" y1="' + pad + '" x2="' + (pad + c) + '" y2="' + (H - pad) + '"/>';
        }
        for (var i = 0; i < 42; i++) {
            var cx = pad + (i % 7) + 0.5, cy = pad + Math.floor(i / 7) + 0.5, m = board[i];
            if (m !== 'A' && m !== 'B') {
                s += '<circle class="gp-slot" cx="' + cx + '" cy="' + cy + '" r="0.35"/>';
                continue;
            }
            var drop = i === lastCell;
            if (drop) s += '<circle class="gp-slot" cx="' + cx + '" cy="' + cy + '" r="0.35"/>';
            s += '<g class="gp-token' + (drop ? ' gp-drop' : '') + '"' +
                (drop ? ' style="--gp-fall:-' + (Math.floor(i / 7) + 1) + 'px"' : '') + '>';
            if (m === 'A') {
                s += '<circle class="gp-a" cx="' + cx + '" cy="' + cy + '" r="0.37"/><circle class="gp-a-dot" cx="' + cx + '" cy="' + cy + '" r="0.12"/>';
            } else {
                s += '<circle class="gp-b" cx="' + cx + '" cy="' + cy + '" r="0.31" fill="url(#' + id + ')"/>' +
                    '<rect class="gp-b-mark" x="' + (cx - 0.1) + '" y="' + (cy - 0.1) + '" width="0.2" height="0.2" transform="rotate(45 ' + cx + ' ' + cy + ')"/>';
            }
            s += '</g>';
        }
        return s + '</svg>';
    }

    // board: 9 cells of X, O, or _.
    function ttt(board, drawCell, extra) {
        board = typeof board === 'string' && board.length >= 9 ? board : SAMPLE_TTT;
        var g = 0.09, size = 3 + 4 * g;
        var s = '<svg class="gp gp-ttt' + (extra ? ' ' + extra : '') + '" viewBox="0 0 ' + size + ' ' + size +
            '" aria-hidden="true" focusable="false">';
        s += '<rect class="gp-ttt-frame" x="0" y="0" width="' + size + '" height="' + size + '" rx="0.24"/>';
        for (var i = 0; i < 9; i++) {
            var x = g + (i % 3) * (1 + g), y = g + Math.floor(i / 3) * (1 + g), m = board[i];
            s += '<rect class="gp-ttt-cell" x="' + x + '" y="' + y + '" width="1" height="1" rx="0.12"/>';
            if (m === 'X') {
                s += '<g class="gp-x' + (i === drawCell ? ' gp-draw' : '') + '"><line x1="' + (x + 0.28) + '" y1="' + (y + 0.28) + '" x2="' +
                    (x + 0.72) + '" y2="' + (y + 0.72) + '"/><line x1="' + (x + 0.72) + '" y1="' + (y + 0.28) + '" x2="' +
                    (x + 0.28) + '" y2="' + (y + 0.72) + '"/></g>';
            } else if (m === 'O') {
                s += '<circle class="gp-o" cx="' + (x + 0.5) + '" cy="' + (y + 0.5) + '" r="0.25"/>';
            }
        }
        return s + '</svg>';
    }

    function value(session, key) {
        if (!session) return undefined;
        if (session[key] !== undefined && session[key] !== null) return session[key];
        return session.metadata ? session.metadata[key] : undefined;
    }

    // The sample art for a game: 'wide' (gallery stage), 'square' (row tile).
    function preview(appId, variant) {
        var square = variant === 'square';
        if (appId === 'chess') {
            return square ? chess([2, 2, 4, 4], SAMPLE_FEN, [5, 7, 2, 4], 'gp-square')
                : chess([0, 2, 8, 4], SAMPLE_FEN, [5, 7, 2, 4], 'gp-wide');
        }
        if (appId === 'four_in_a_row') return four(SAMPLE_FOUR, 18, square ? 'gp-square' : 'gp-wide');
        if (appId === 'ttt') return ttt(SAMPLE_TTT, 6, square ? 'gp-square' : 'gp-wide');
        return '';
    }

    // The current position of a session, for thumbnails.
    function position(session) {
        var appId = session && (session.app_id || session.game);
        if (!session || session.status === 'pending') return preview(appId, 'square');
        if (appId === 'chess') {
            return chess([2, 2, 4, 4], value(session, 'fen') || 'rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR',
                uciSquares(value(session, 'last_move')), 'gp-square');
        }
        if (appId === 'four_in_a_row') {
            var lastCell = parseInt(value(session, 'last_cell'), 10);
            return four(value(session, 'board'), isNaN(lastCell) ? -1 : lastCell, 'gp-square');
        }
        if (appId === 'ttt') return ttt(value(session, 'state') || value(session, 'board') || '_________', -1, 'gp-square');
        return '';
    }

    function has(appId) {
        return appId === 'chess' || appId === 'four_in_a_row' || appId === 'ttt';
    }

    RS.games.art = Object.freeze({
        has: has,
        preview: preview,
        position: position,
        fenPieces: fenPieces,
    });
})(window);
