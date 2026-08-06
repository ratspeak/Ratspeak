#!/usr/bin/env node
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var registrySource = fs.readFileSync(
    path.join(__dirname, '..', 'static', 'js', 'game_registry.js'),
    'utf8'
);
var viewSource = fs.readFileSync(
    path.join(__dirname, '..', 'static', 'js', 'four_in_a_row_view.js'),
    'utf8'
);

var documentStub = { documentElement: {} };
var windowStub = {
    RS: {},
    document: documentStub,
    getComputedStyle: function() {
        return { getPropertyValue: function() { return ''; } };
    },
};
var context = {
    window: windowStub,
    document: documentStub,
    Object: Object,
    TypeError: TypeError,
    Error: Error,
    Date: Date,
};
vm.createContext(context);
vm.runInContext(registrySource, context, { filename: 'game_registry.js' });
vm.runInContext(viewSource, context, { filename: 'four_in_a_row_view.js' });

var views = windowStub.RS.games.views;
var adapter = views.get('four_in_a_row');
assert(adapter, 'Four in a Row adapter must register under its canonical app_id');
assert.strictEqual(adapter.icon, '\u25cf\u25cf\u25cf\u25cf');
assert.strictEqual(adapter.boardSelector, '.four-board');
assert.deepStrictEqual(Array.from(adapter.actions), [
    'challenge', 'accept', 'decline', 'move', 'resign',
    'draw_offer', 'draw_accept', 'draw_decline'
]);

function session(overrides) {
    var base = {
        game_id: 'four-test',
        app_id: 'four_in_a_row',
        status: 'active',
        board: '__________________________________________',
        turn: 'me',
        first_turn: 'me',
        my_marker: 'A',
        move_count: 0,
        last_column: null,
        last_row: null,
        last_cell: null,
        winner: '',
        terminal: '',
        contact_hash: 'them',
        challenger: 'me',
    };
    Object.keys(overrides || {}).forEach(function(key) { base[key] = overrides[key]; });
    return base;
}

function metadataSession(overrides) {
    var record = session();
    var fields = [
        'board', 'turn', 'first_turn', 'my_marker', 'move_count',
        'last_column', 'last_row', 'last_cell', 'winner', 'terminal'
    ];
    record.metadata = {};
    fields.forEach(function(field) {
        record.metadata[field] = record[field];
        delete record[field];
    });
    Object.keys(overrides || {}).forEach(function(key) {
        record.metadata[key] = overrides[key];
    });
    return record;
}

function viewContext(extra) {
    var base = {
        isMe: function(hash) { return hash === 'me'; },
        myHash: function() { return 'me'; },
        contactName: function() { return '<Rival>'; },
    };
    Object.keys(extra || {}).forEach(function(key) { base[key] = extra[key]; });
    return base;
}

var emptyHtml = adapter.renderBoard(session(), viewContext());
assert.strictEqual((emptyHtml.match(/role="gridcell"/g) || []).length, 42,
    'the board must expose all 42 authoritative cells');
assert(emptyHtml.includes('aria-rowindex="1" aria-colindex="1"'));
assert(emptyHtml.includes('aria-rowindex="6" aria-colindex="7"'));
assert(!/role="gridcell"[^>]*tabindex=/.test(emptyHtml),
    'read-only cells must stay out of the tab order');
assert.strictEqual((emptyHtml.match(/class="four-lane-action"/g) || []).length, 7,
    'the action layer must expose one lane target per column');
assert.strictEqual((emptyHtml.match(/class="four-lane-action"[^>]* disabled/g) || []).length, 0,
    'all seven columns start playable');
assert(emptyHtml.includes('aria-label="Drop Node in column 1; lands in row 6"'));
assert(emptyHtml.includes('&lt;Rival&gt;'), 'opponent names must be escaped');
assert(!emptyHtml.includes('<Rival>'), 'raw opponent markup must never render');

var metadataHtml = adapter.renderBoard(metadataSession({
    board: '_________________________________________A',
    turn: 'them',
    my_marker: 'B',
    last_column: 6,
}), viewContext());
assert(metadataHtml.includes('four-token four-token-a'),
    'the canonical metadata board must drive rendering');
assert(metadataHtml.includes('Column 1; wait for your turn'),
    'disabled lanes must explain turn ownership rather than claim they are full');
assert.strictEqual(adapter.activeStatusText(metadataSession({
    turn: 'them', my_marker: 'B', first_turn: 'them'
}), viewContext()), '<Rival>\u2019s turn · Node');
assert.deepStrictEqual(
    Array.from(adapter.detailChips(metadataSession({
        my_marker: 'B', last_column: 6
    }), viewContext())),
    ['You: Ring', 'Last: column 7']
);

var mixed = '___________________________________AB_____';
var mixedHtml = adapter.renderBoard(session({ board: mixed }), viewContext());
assert(mixedHtml.includes('four-token four-token-a'), 'A must render as a solid-node class');
assert(mixedHtml.includes('four-token four-token-b'), 'B must render as a patterned-ring class');

var fullColumnBoard = 'A______A______B______B______A______A______';
var fullHtml = adapter.renderBoard(session({ board: fullColumnBoard }), viewContext());
assert.strictEqual((fullHtml.match(/class="four-lane-action"[^>]* disabled/g) || []).length, 1,
    'a full column must remove exactly that lane from the tab and click order');
assert(fullHtml.includes('aria-label="Column 1 is full" disabled'));

var winningBoard = '___________________________________AAAA___';
var winHtml = adapter.renderBoard(session({
    board: winningBoard,
    status: 'completed',
    terminal: 'win',
    winner: 'me',
}), viewContext());
assert.strictEqual((winHtml.match(/ winning/g) || []).length, 4,
    'exactly the winning connection must be emphasized');
assert(winHtml.includes('class="four-win-trace animate"'),
    'the confirmed winning trace animates once');
assert(winHtml.includes('You won!'));
var repeatedWinHtml = adapter.renderBoard(session({
    board: winningBoard,
    status: 'completed',
    terminal: 'win',
    winner: 'me',
}), viewContext());
assert(repeatedWinHtml.includes('class="four-win-trace"'));
assert(!repeatedWinHtml.includes('class="four-win-trace animate"'),
    'ordinary rerenders must leave the winning trace static');

var metadataWinner = metadataSession({
    board: winningBoard,
    terminal: 'win',
    winner: 'me',
});
metadataWinner.status = 'completed';
var metadataWinHtml = adapter.renderBoard(metadataWinner, viewContext());
assert(metadataWinHtml.includes('You won!'),
    'canonical metadata must also drive terminal presentation');

var outgoingPending = metadataSession();
outgoingPending.status = 'pending';
outgoingPending.initiator = 'me';
delete outgoingPending.challenger;
assert(adapter.renderBoard(outgoingPending, viewContext()).includes('Waiting for response…'),
    'pending ownership must accept the normalized initiator projection');
var incomingPending = metadataSession();
incomingPending.status = 'pending';
incomingPending.initiator = 'them';
delete incomingPending.challenger;
assert(adapter.renderBoard(incomingPending, viewContext()).includes('Challenge received!'));

var resignedWinner = metadataSession({ terminal: 'resign', winner: 'me' });
resignedWinner.status = 'completed';
assert(adapter.renderBoard(resignedWinner, viewContext()).includes('Opponent resigned'));
var resignedLocal = metadataSession({ terminal: 'resign', winner: 'them' });
resignedLocal.status = 'completed';
assert(adapter.renderBoard(resignedLocal, viewContext()).includes('You resigned'));

assert.strictEqual(adapter.activeStatusText(session(), viewContext()), 'Your turn · Node');
assert.strictEqual(adapter.activeStatusText(session({
    turn: 'them', my_marker: 'B', first_turn: 'them'
}), viewContext()), '<Rival>\u2019s turn · Node');
assert.deepStrictEqual(
    Array.from(adapter.detailChips(session({ my_marker: 'B', last_column: 6 }), viewContext())),
    ['You: Ring', 'Last: column 7']
);

function fakeElement(attributes) {
    return {
        attributes: attributes || {},
        handlers: {},
        focused: false,
        classList: { add: function() {}, remove: function() {} },
        getAttribute: function(name) { return String(this.attributes[name]); },
        addEventListener: function(name, handler) { this.handlers[name] = handler; },
        focus: function() { this.focused = true; },
    };
}

var laneElements = [];
for (var column = 0; column < 7; column++) {
    laneElements.push(fakeElement({ 'data-column': column, 'data-landing-row': 5 }));
}
var previewCell = fakeElement({});
var root = {
    querySelectorAll: function(selector) {
        if (selector === '.four-lane-action:not(:disabled)') return laneElements;
        if (selector === '.four-cell.is-preview') return [];
        return [];
    },
    querySelector: function(selector) {
        return selector.indexOf('.four-cell.four-landing-cell') === 0 ? previewCell : null;
    },
};
var sent = null;
var played = session();
adapter.bindBoard(played, viewContext({
    root: root,
    sendMove: function(payload, optimistic) {
        sent = { payload: payload, optimistic: optimistic };
        optimistic.apply(played);
        return true;
    },
}));
laneElements[2].handlers.click();
assert.deepStrictEqual(JSON.parse(JSON.stringify(sent.payload)), { c: 2 },
    'the adapter must send only the local column intent');
assert.deepStrictEqual(Array.from(sent.optimistic.fields), [
    'board', 'last_column', 'last_row', 'last_cell'
]);
assert.strictEqual(played.board[37], 'A');
assert.strictEqual(played.last_column, 2);
assert.strictEqual(played.last_row, 5);
assert.strictEqual(played.last_cell, 37);
assert.strictEqual(played.move_count, 1);
assert.strictEqual(played.turn, 'them');

var prevented = false;
laneElements[2].handlers.keydown({
    key: 'ArrowRight',
    preventDefault: function() { prevented = true; },
});
assert(prevented, 'lane arrow navigation must prevent page scrolling');
assert(laneElements[3].focused, 'ArrowRight must focus the next legal lane');

console.log('Four in a Row view tests passed');
