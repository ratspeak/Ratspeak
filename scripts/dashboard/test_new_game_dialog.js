#!/usr/bin/env node
'use strict';

// New game (option B): the kit Large dialog, a radio gallery of board tiles,
// searchable opponents, and an explicit Primary.
var assert = require('assert');
var fs = require('fs');
var path = require('path');
var vm = require('vm');

var dashboardRoot = path.join(__dirname, '..', '..', 'dashboard');
var source = fs.readFileSync(path.join(dashboardRoot, 'static', 'js', 'games_tab.js'), 'utf8');
var art = fs.readFileSync(path.join(dashboardRoot, 'static', 'js', 'game_art.js'), 'utf8');
var gamesCss = fs.readFileSync(path.join(dashboardRoot, 'static', 'css', '11-games.css'), 'utf8');
var responsiveCss = fs.readFileSync(path.join(dashboardRoot, 'static', 'css', '13-responsive.css'), 'utf8');

function functionSource(name) {
    var start = source.indexOf('function ' + name + '(');
    assert.notStrictEqual(start, -1, name + ' must exist');
    var depth = 0;
    for (var i = source.indexOf('{', start); i < source.length; i++) {
        if (source[i] === '{') depth += 1;
        if (source[i] === '}' && --depth === 0) return source.slice(start, i + 1);
    }
    throw new Error('unterminated ' + name);
}

// The kit dialog and its parts.
assert(/RS\.kit\.dialog\(\{\s*size: 'lg',\s*className: 'ng-dialog',/.test(source),
    'New game is the kit Large dialog (a bottom sheet on a phone)');
assert(source.includes('<div class="ng-gallery" role="radiogroup" aria-labelledby="ng-gl">'),
    'the games form one radio group');
assert(source.includes('<input class="game-choice-input" type="radio" name="ng-game"'));
assert(source.includes('<div class="ng-people ui-scroll"><div class="ng-opp-head ui-scroll-head">') &&
    source.includes('<div class="ng-people-list" role="radiogroup" aria-labelledby="ng-ol">'),
    'the opponent header scrolls inside the list as a sticky head');
assert(source.includes('placeholder="Search contacts" aria-label="Search contacts"'));
assert(source.includes("'<p class=\"ui-caption\">Recent</p>'") && source.includes("'<p class=\"ui-caption\">Contacts</p>'"));
assert(source.includes('No contacts yet') && source.includes('Games are played with contacts. Add someone first.') &&
    source.includes('>Open Contacts</button>'), 'the no-contact state has a next step');
assert(source.includes("switchView('contacts', { pushState: true })"));
assert(source.includes('Search checks names and the start of addresses.') && source.includes('>Clear search</button>'));
assert(source.includes("'Games need Ratspeak'"), 'contacts without Ratspeak are shown but cannot be picked');
assert(source.includes('class="ui-btn ui-btn--quiet ui-desk-only ng-cancel">Cancel</button>'),
    'Cancel is desktop only; the sheet does not need it');
assert(source.includes("'<span class=\"ng-send-label\"><span class=\"f\">Challenge </span><span class=\"n\">'"),
    'the Primary names the opponent and the game');
assert(source.includes("'<span class=\"ui-spin\"></span>Sending challenge\u2026'"), 'the spinner sits in the button while sending');
['Choose a game and an opponent.', 'Choose a game.', 'Choose an opponent.', 'They have 24 hours to accept.',
    'Add a contact to start a game.'].forEach(function(hint) {
    assert(source.includes("'" + hint + "'"), 'missing hint: ' + hint);
});
assert(source.includes("send.disabled = state.sending || !(state.game && state.opponent);"),
    'the kit faded Primary until a game and an opponent are chosen');
assert(!source.includes('function _presentModal') && !source.includes("addEventListener('keydown', onKey"),
    'one dialog shell: RS.kit.dialog owns Escape, Tab, the scrim, native Back, and the drag');
assert(/RS\.kit\.dialog\(\{\s*className: 'game-result-dialog',/.test(source), 'the phone result sheet is the kit dialog too');
assert(!/ui-modal|games-modal/.test(gamesCss) && !gamesCss.includes('gameSheetUp'),
    'one dialog animation: the kit\'s');
assert(source.includes('openNewGame({') && source.includes("gameId: this.getAttribute('data-new-game') || null") &&
    source.includes("contactHash: this.getAttribute('data-opponent') || null"), 'landing tiles and Play again pre-choose');
assert(source.includes('window.openNewGame = openNewGame;'), 'a chat starts a game through the same entry');
assert(!source.includes('games-sheet-') && !source.includes('bottom-sheet games-new-dialog'),
    'the old New game sheet is gone');

// Only contacts heard without Ratspeak are off; unknown contacts can still be challenged.
var canPlay = vm.runInNewContext('(' + functionSource('_canPlay') + ')');
assert.strictEqual(canPlay({ services: ['lxmf.delivery', 'ratspeak.client'] }), true);
assert.strictEqual(canPlay({ supports_ratspeak: true, services: [] }), true);
assert.strictEqual(canPlay({ services: [] }), true);
assert.strictEqual(canPlay({}), true);
assert.strictEqual(canPlay({ services: ['lxmf.delivery'] }), false);

// The board art is drawn from the live boards' tokens and the chess sprite.
assert(art.includes("var SPRITE = '/static/assets/chess-pieces.svg#';"));
['preview: preview', 'position: position'].forEach(function(entry) { assert(art.includes(entry)); });

// Layout: three tiles in a row on desktop; a two-step sheet on a phone.
assert(/\.ng-gallery\s*\{[^}]*grid-template-columns:\s*repeat\(3,\s*minmax\(0,\s*1fr\)\)/.test(gamesCss));
assert(/\.ui-pane-head\.games-pane-head\s*\{\s*padding-right:\s*var\(--ui-gutter\);/.test(gamesCss),
    'the Games header button ends on the content edge');
assert(!/scrollbar-width|::-webkit-scrollbar/.test(gamesCss), 'scrollbars follow the shared rules');
assert(/\.game-choice:has\(\.game-choice-input:checked\)\s*\{\s*background:\s*var\(--selection-bg\);\s*\}/.test(gamesCss),
    'a chosen tile takes the selection fill, never an accent border');
assert(!/\.game-choice[^{]*\{[^}]*border:(?!\s*0\b)/.test(gamesCss), 'choice tiles carry no border');
assert(/\.ng-step-pick \.ng-picks\s*\{\s*display:\s*flex;/.test(gamesCss) &&
    /\.ng-step-pick \.ng-opp, \.ng-step-pick \.ui-dialog-foot\s*\{\s*display:\s*none;/.test(gamesCss),
    'a phone picks the game first, then the opponent');
assert(/\.ng-send-label \.n\s*\{[^}]*text-overflow:\s*ellipsis/.test(gamesCss) &&
    /\.ng-send-label \.f\s*\{[^}]*white-space:\s*pre/.test(gamesCss), 'the name truncates; the game never does');
assert(/prefers-reduced-motion:\s*reduce[\s\S]*?\.game-choice,[\s\S]*?animation:\s*none !important/.test(gamesCss));
assert(!responsiveCss.includes('games-new-dialog') && !responsiveCss.includes('games-sheet-'),
    'no dead New game sheet rules remain');

console.log('New game dialog tests passed');
