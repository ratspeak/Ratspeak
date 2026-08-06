#!/usr/bin/env node
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var source = fs.readFileSync(
    path.join(__dirname, '..', 'static', 'js', 'game_registry.js'),
    'utf8'
);
var context = { window: { RS: {} }, Object: Object, TypeError: TypeError, Error: Error };
vm.createContext(context);
vm.runInContext(source, context, { filename: 'game_registry.js' });

var views = context.window.RS.games.views;
assert(views, 'game view registry must be exported');
assert.deepStrictEqual(Array.from(views.listIds()), []);

var rendered = 0;
var bound = 0;
var fleet = views.register('fleet', {
    displayName: 'Fleet',
    icon: '\u2693',
    themeClass: 'games-theme-fleet',
    boardSelector: '.fleet-board',
    actions: ['challenge', 'accept', 'move', 'resign'],
    renderBoard: function(session) {
        rendered += 1;
        return '<div>' + session.game_id + '</div>';
    },
    bindBoard: function() { bound += 1; },
    activeStatusText: function() { return 'Your salvo'; },
    detailChips: function() { return ['Fleet 2']; },
    renderActiveControls: function() { return '<button>Scan</button>'; },
    bindControls: function() { bound += 1; },
});

assert.strictEqual(views.has('fleet'), true);
assert.strictEqual(views.get('fleet'), fleet);
assert.strictEqual(fleet.displayName, 'Fleet');
assert.deepStrictEqual(Array.from(fleet.actions), ['challenge', 'accept', 'move', 'resign']);
assert.strictEqual(Object.isFrozen(fleet.actions), true);
assert.strictEqual(fleet.renderBoard({ game_id: 'third-game' }), '<div>third-game</div>');
fleet.bindBoard({});
assert.strictEqual(rendered, 1);
assert.strictEqual(bound, 1);
assert.strictEqual(fleet.activeStatusText({}), 'Your salvo');
assert.deepStrictEqual(Array.from(fleet.detailChips({})), ['Fleet 2']);
assert.strictEqual(fleet.renderActiveControls({}), '<button>Scan</button>');
fleet.bindControls({});
assert.strictEqual(bound, 2);
assert.strictEqual(Object.isFrozen(fleet), true);
assert.deepStrictEqual(Array.from(views.listIds()), ['fleet']);
var selectable = views.supportedManifests([
    { app_id: 'unknown', display_name: 'Unsupported' },
    { app_id: 'fleet', display_name: 'Fleet' },
]);
assert.deepStrictEqual(
    Array.from(selectable, function(manifest) { return manifest.app_id; }),
    ['fleet']
);

assert.throws(function() {
    views.register('fleet', { renderBoard: function() {}, bindBoard: function() {} });
}, /already registered/);
assert.throws(function() {
    views.register('Fleet', { renderBoard: function() {}, bindBoard: function() {} });
}, /canonical LRGP grammar/);
assert.throws(function() {
    views.register('missing_view', { renderBoard: function() {} });
}, /renderBoard and bindBoard/);

views.register('cards', {
    renderBoard: function() { return ''; },
    bindBoard: function() {},
});
assert.deepStrictEqual(Array.from(views.listIds()), ['cards', 'fleet']);
assert.deepStrictEqual(Array.from(views.supportedManifests(null)), []);

var gameState = context.window.RS.games.state;
assert(gameState, 'shared session-state helpers must be exported');
assert.strictEqual(gameState.value({ turn: 'root', metadata: { turn: 'meta' } }, 'turn', ''), 'root');
assert.strictEqual(gameState.value({ metadata: { turn: 'meta' } }, 'turn', ''), 'meta');
assert.strictEqual(gameState.value({ metadata: {} }, 'turn', 'fallback'), 'fallback');

var optimistic = context.window.RS.games.optimistic;
assert(optimistic, 'shared optimistic-state helpers must be exported');
var liveState = { board: 'before', last_column: null };
var snapshot = optimistic.captureFields(liveState, [
    'board', 'last_column', 'last_row', 'last_cell'
]);
liveState.board = 'after';
liveState.last_column = 4;
liveState.last_row = 5;
liveState.last_cell = 39;
optimistic.restoreFields(liveState, snapshot);
assert.deepStrictEqual(
    JSON.parse(JSON.stringify(liveState)),
    { board: 'before', last_column: null },
    'rollback must restore existing fields and remove optimistic-only fields'
);

console.log('game view registry tests passed');
