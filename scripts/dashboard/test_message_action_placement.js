// Placement of the message menu: on desktop one kit menu under its anchor,
// above it when there is no room, docked in the transcript band otherwise,
// never over the header or composer, aligned to the message's own side. On a
// phone a sheet, with the pressed message scrolled into view above it.
const assert = require('assert');
const fs = require('fs');
const path = require('path');
const vm = require('vm');

const root = path.resolve(__dirname, '..', '..');
const source = fs.readFileSync(path.join(root, 'dashboard/static/js/lxmf.js'), 'utf8');
const start = source.indexOf('var MSG_ACTION_GAP = 8;');
const end = source.indexOf('function _prepareMessageActionTarget');
assert(start > 0 && end > start, 'placement block must stay extractable');

const VIEW_W = 1000;
const VIEW_H = 800;
const TOP_BOTTOM = 64;
const COMPOSER_TOP = 730;
const MENU_H = 200;
const MENU_W = 248;

function rect(top, height, left, width) {
    return { top: top, bottom: top + height, left: left, right: left + width, height: height, width: width };
}

function scenario(bubbleTop, bubbleHeight, opts) {
    opts = opts || {};
    const scroller = {
        _top: opts.scrollTop || 0,
        max: opts.maxScroll === undefined ? 10000 : opts.maxScroll,
        style: {},
        get scrollTop() { return this._top; },
        set scrollTop(v) { this._top = Math.max(0, Math.min(this.max, v)); },
    };
    const bubbleLeft = opts.outbound ? 600 : 300;
    const bubble = {
        isConnected: true,
        getBoundingClientRect: () => rect(bubbleTop - (scroller._top - (opts.scrollTop || 0)), bubbleHeight, bubbleLeft, 300),
    };
    const style = { setProperty: function(k, v) { this[k] = v; } };
    const menuAttrs = {};
    const sheetTop = opts.sheetTop || 500;
    const menu = {
        offsetHeight: MENU_H, offsetWidth: MENU_W, style: style,
        setAttribute: (k, v) => { menuAttrs[k] = v; },
        getBoundingClientRect: () => rect(sheetTop, VIEW_H - sheetTop, 0, VIEW_W),
    };
    const elements = {
        'lxmf-chat-top': { hidden: false, getBoundingClientRect: () => rect(0, TOP_BOTTOM, 0, VIEW_W) },
        'lxst-call-strip': { hidden: true, getBoundingClientRect: () => rect(0, 0, 0, 0) },
        'lxmf-composer': { hidden: false, getBoundingClientRect: () => rect(COMPOSER_TOP, VIEW_H - COMPOSER_TOP, 0, VIEW_W) },
        'lxmf-messages': scroller,
    };
    const state = {
        bubble: bubble,
        row: null,
        outbound: !!opts.outbound,
        menu: menu,
        sheet: !!opts.sheet,
        anchorRect: opts.anchor ? opts.anchor(bubble.getBoundingClientRect()) : null,
    };
    const insets = opts.insets || { top: 0, right: 0, bottom: 0, left: 0 };
    const context = {
        window: { innerWidth: VIEW_W, innerHeight: VIEW_H, visualViewport: null },
        document: { getElementById: (id) => elements[id] || null },
        safeAreaInsets: () => insets,
        Math: Math,
    };
    vm.createContext(context);
    vm.runInContext(source.slice(start, end), context);
    const placement = context._layoutMessageActionPanels(state, true);
    return {
        placement: placement,
        attr: menuAttrs['data-placement'],
        menu: { top: parseFloat(style.top), left: parseFloat(style.left) },
        bubble: bubble.getBoundingClientRect(),
        scrollTop: scroller._top,
    };
}

const bandTop = TOP_BOTTOM + 8;
const bandBottom = COMPOSER_TOP - 8;
function insideBand(r) {
    assert(r.menu.top >= bandTop, 'the menu never covers the header');
    assert(r.menu.top + MENU_H <= bandBottom, 'the menu never covers the composer');
}

// Room below: under the message (or its anchor).
let r = scenario(200, 60);
assert.strictEqual(r.placement, 'below');
assert.strictEqual(r.attr, 'below');
assert.strictEqual(r.menu.top, r.bubble.bottom + 4);
insideBand(r);

// Near the composer: above it.
r = scenario(560, 60);
assert.strictEqual(r.placement, 'above');
assert.strictEqual(r.menu.top + MENU_H, r.bubble.top - 4);
insideBand(r);

// Anchored to the hover bar's More: under the button, not the bubble.
r = scenario(200, 120, { anchor: (b) => rect(b.top + 40, 32, b.right + 8, 32) });
assert.strictEqual(r.menu.top, 200 + 40 + 32 + 4);

// Scrolled up under the header (opened from the keyboard): the menu stays in the band.
r = scenario(-200, 60);
assert.strictEqual(r.placement, 'below');
insideBand(r);

// Taller than the room around it: docked in the band.
r = scenario(-300, 1400);
assert.strictEqual(r.placement, 'dock');
insideBand(r);

// Side alignment: inbound left edges align; outbound right edges align.
r = scenario(200, 60);
assert.strictEqual(r.menu.left, r.bubble.left);
r = scenario(200, 60, { outbound: true });
assert.strictEqual(r.menu.left + MENU_W, r.bubble.right);

// Side insets (a side status bar, a landscape notch) shrink the usable width.
r = scenario(200, 60, { outbound: true, insets: { top: 0, right: 30, bottom: 0, left: 0 } });
assert(r.menu.left + MENU_W <= VIEW_W - 12 - 30, 'the menu stays clear of a right safe-area inset');
r = scenario(200, 60, { insets: { top: 0, right: 0, bottom: 0, left: 320 } });
assert(r.menu.left >= 12 + 320, 'the menu stays clear of a left safe-area inset');

// Phone sheet: the pressed message rises above the sheet by the smallest shift, never under the header.
r = scenario(520, 60, { sheet: true, sheetTop: 500, scrollTop: 400 });
assert.strictEqual(r.placement, 'sheet');
assert.strictEqual(r.bubble.bottom, 500 - 8, 'the message sits just above the sheet');
r = scenario(200, 60, { sheet: true, sheetTop: 500, scrollTop: 400 });
assert.strictEqual(r.scrollTop, 400, 'a message already in view does not move');
r = scenario(30, 60, { sheet: true, sheetTop: 500, scrollTop: 400 });
assert.strictEqual(r.bubble.top, bandTop, 'a message under the header comes down into view');
r = scenario(100, 900, { sheet: true, sheetTop: 500, scrollTop: 400 });
assert.strictEqual(r.bubble.top, bandTop, 'a message taller than the room shows its start');

// Arrow keys follow on-screen order: reactions wrap Left/Right, rows move
// Up/Down, and Up/Down at an edge crosses into the neighbouring group.
const keys = { window: {}, document: {}, safeAreaInsets: () => ({ top: 0, right: 0, bottom: 0, left: 0 }), Math: Math };
vm.createContext(keys);
vm.runInContext(source.slice(start, end), keys);
const move = keys._messageActionKeyTarget;
const react = ['heart', 'thumbs', 'more'];
const rows = ['reply', 'select', 'copy'];
const splitOrder = [{ items: react, horizontal: true }, { items: rows, horizontal: false }];
assert.strictEqual(move(splitOrder, 'heart', 'ArrowRight'), 'thumbs');
assert.strictEqual(move(splitOrder, 'heart', 'ArrowLeft'), 'more', 'reactions wrap');
assert.strictEqual(move(splitOrder, 'more', 'ArrowRight'), 'heart');
assert.strictEqual(move(splitOrder, 'thumbs', 'ArrowDown'), 'reply');
assert.strictEqual(move(splitOrder, 'thumbs', 'ArrowUp'), null);
assert.strictEqual(move(splitOrder, 'reply', 'ArrowDown'), 'select');
assert.strictEqual(move(splitOrder, 'copy', 'ArrowDown'), null, 'rows do not wrap');
assert.strictEqual(move(splitOrder, 'reply', 'ArrowUp'), 'heart', 'entering reactions starts at the first');
assert.strictEqual(move(splitOrder, 'select', 'ArrowLeft'), null);
assert.strictEqual(move(splitOrder, 'select', 'End'), 'copy');
assert.strictEqual(move(splitOrder, 'thumbs', 'Home'), 'heart');
const aboveOrder = [{ items: rows, horizontal: false }, { items: react, horizontal: true }];
assert.strictEqual(move(aboveOrder, 'copy', 'ArrowDown'), 'heart', 'reactions below the card are reached downward');
assert.strictEqual(move(aboveOrder, 'heart', 'ArrowUp'), 'copy', 'entering rows upward lands on the nearest row');
assert.strictEqual(move(splitOrder, 'elsewhere', 'ArrowDown'), null);

console.log('Message action placement tests passed.');
