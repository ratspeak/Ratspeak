#!/usr/bin/env node
'use strict';

var assert = require('assert');
var fs = require('fs');
var path = require('path');

var dashboardRoot = path.join(__dirname, '..');
var source = fs.readFileSync(
    path.join(dashboardRoot, 'static', 'js', 'games_tab.js'),
    'utf8'
);
var gamesCss = fs.readFileSync(
    path.join(dashboardRoot, 'static', 'css', '11-games.css'),
    'utf8'
);
var responsiveCss = fs.readFileSync(
    path.join(dashboardRoot, 'static', 'css', '13-responsive.css'),
    'utf8'
);
var fourSource = fs.readFileSync(
    path.join(dashboardRoot, 'static', 'js', 'four_in_a_row_view.js'),
    'utf8'
);

assert(source.includes('Choose what to play and who to challenge.'));
assert(source.includes("setAttribute('role', 'dialog')"));
assert(source.includes("setAttribute('aria-labelledby', 'games-new-sheet-title')"));
assert(source.includes('games-sheet-game-check'));
assert(source.includes('Add someone before starting a game.'));
assert(source.includes("switchView('contacts', { pushState: true })"),
    'the no-contact state must provide a useful next step');
assert(source.includes("if (e.key === 'Escape')"),
    'the desktop dialog must be keyboard dismissible');
assert(source.includes("sheet.querySelectorAll('button:not([disabled])')"),
    'keyboard focus must stay within the open dialog');
assert(!source.includes('_gameChoiceHint') && !source.includes('games-sheet-game-hint'),
    'game choices must not display redundant board dimensions');
assert(source.includes('games-four-icon-mark') &&
    /grid-template-columns:\s*repeat\(4,\s*5px\)/.test(gamesCss),
    'Four in a Row must use a stable four-disc mark');
assert(!fourSource.includes('\\u283F'),
    'Four in a Row must not fall back to the ambiguous six-dot Braille glyph');
assert(!/data-app-id="chess"[^}]*ble-accent-fg/.test(gamesCss),
    'the Chess choice must use the theme accent instead of connectivity blue');

assert(/\.games-sheet-game-grid\s*\{[\s\S]*?grid-template-columns:\s*repeat\(3,\s*minmax\(0,\s*1fr\)\)/.test(gamesCss),
    'the three built-in games must form one balanced desktop row');
assert(/\.games-sheet-game-card\.selected\s*\{[\s\S]*?border-color:\s*var\(--accent\)/.test(gamesCss),
    'selected games must retain a clear theme-native state');
assert(!/\.games-sheet-game-card\.selected\s*\{[^}]*inset\s+0\s+0\s+0\s+1px/s.test(gamesCss),
    'selection must not use the old heavy double outline');
assert(/\.games-sheet-contact-list\.is-empty\s*\{/.test(gamesCss) &&
    /\.games-sheet-open-contacts\s*\{/.test(gamesCss),
    'the empty opponent state must be intentionally designed');
assert(/\.games-sheet-game-grid\s*\{[\s\S]*?repeat\(3,\s*minmax\(0,\s*1fr\)\)/.test(responsiveCss),
    'mobile must retain all three compact game choices without an orphan row');
assert(/\.games-sheet-footer\s*\{[\s\S]*?width:\s*100%[\s\S]*?flex-wrap:\s*nowrap/.test(responsiveCss),
    'mobile challenge actions must remain inside the viewport');
assert(/\.bottom-sheet\.open\.games-new-dialog\s*\{[\s\S]*?overflow-y:\s*hidden/.test(responsiveCss),
    'large mobile text must scroll the dialog body rather than hide footer actions');
assert(/prefers-reduced-motion:[^)]*reduce[\s\S]*?\.games-sheet-game-card[\s\S]*?transition:\s*none\s*!important/.test(gamesCss),
    'game-choice motion must honor reduced motion');

console.log('New game dialog tests passed');
