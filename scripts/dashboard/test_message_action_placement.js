// Placement of the message reactions capsule and action card around the
// selected message: split > below > above > scroll-to-fit > dock, never over
// the header or composer, aligned to the message's own side.
const assert = require('assert');
const fs = require('fs');
const path = require('path');
const vm = require('vm');

const root = path.resolve(__dirname, '..', '..');
const source = fs.readFileSync(path.join(root, 'dashboard/static/js/lxmf.js'), 'utf8');
const start = source.indexOf('var MSG_ACTION_GAP = 8;');
const end = source.indexOf('function _prepareMessageActionTarget');
assert(start > 0 && end > start, 'placement block must stay extractable');

const VIEW_W = 375;
const VIEW_H = 812;
const HEADER_BOTTOM = 56;
const COMPOSER_TOP = 750;
const REACT_H = 52;
const ACTIONS_H = 98;

function rect(top, height, left, width) {
    return { top: top, bottom: top + height, left: left, right: left + width, height: height, width: width };
}

function scenario(bubbleTop, bubbleHeight, opts) {
    opts = opts || {};
    const scroller = {
        _top: opts.scrollTop || 0,
        max: opts.maxScroll === undefined ? 10000 : opts.maxScroll,
        get scrollTop() { return this._top; },
        set scrollTop(v) { this._top = Math.max(0, Math.min(this.max, v)); },
    };
    const bubbleLeft = opts.outbound ? 140 : 16;
    const bubble = {
        isConnected: true,
        getBoundingClientRect: () => rect(bubbleTop - (scroller._top - (opts.scrollTop || 0)), bubbleHeight, bubbleLeft, 220),
    };
    function panel(h, w) {
        const style = { setProperty: function(k, v) { this[k] = v; } };
        return { offsetHeight: h, offsetWidth: w, style: style };
    }
    const elements = {
        'lxmf-chat-header': { hidden: false, getBoundingClientRect: () => rect(0, HEADER_BOTTOM, 0, VIEW_W) },
        'lxst-call-strip': { hidden: true, getBoundingClientRect: () => rect(0, 0, 0, 0) },
        'lxmf-compose-bar': { hidden: false, getBoundingClientRect: () => rect(COMPOSER_TOP, VIEW_H - COMPOSER_TOP, 0, VIEW_W) },
        'lxmf-voice-recorder': { hidden: true, getBoundingClientRect: () => rect(0, 0, 0, 0) },
        'lxmf-messages': scroller,
    };
    const menuAttrs = {};
    const state = {
        bubble: bubble,
        row: null,
        outbound: !!opts.outbound,
        reactPanel: panel(REACT_H, 320),
        actionsPanel: panel(ACTIONS_H, 240),
        menu: { setAttribute: (k, v) => { menuAttrs[k] = v; } },
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
    const px = (p) => ({ top: parseFloat(p.style.top), left: parseFloat(p.style.left), h: p.offsetHeight, w: p.offsetWidth });
    return {
        placement: placement,
        attr: menuAttrs['data-placement'],
        react: px(state.reactPanel),
        actions: px(state.actionsPanel),
        bubble: bubble.getBoundingClientRect(),
        scrollTop: scroller._top,
    };
}

const bandTop = HEADER_BOTTOM + 8;
const bandBottom = COMPOSER_TOP - 8;
function insideBand(r) {
    assert(r.react.top >= bandTop && r.actions.top >= bandTop, 'panels must never cover the header');
    assert(r.react.top + r.react.h <= bandBottom && r.actions.top + r.actions.h <= bandBottom,
        'panels must never cover the composer');
}

// Room on both sides: reactions above, actions below.
let r = scenario(300, 60);
assert.strictEqual(r.placement, 'split');
assert.strictEqual(r.attr, 'split');
assert(r.react.top + r.react.h <= r.bubble.top && r.actions.top >= r.bubble.bottom);
insideBand(r);

// Near the composer at max scroll: flips above, reactions nearest the message.
r = scenario(640, 60, { maxScroll: 0 });
assert.strictEqual(r.placement, 'above');
assert(r.actions.top < r.react.top && r.react.top + r.react.h <= r.bubble.top);
insideBand(r);

// Flipping above never scrolls when it already fits.
r = scenario(640, 60, { scrollTop: 500 });
assert.strictEqual(r.placement, 'above');
assert.strictEqual(r.scrollTop, 500);

// Nothing fits in place but the message fits with its panels: the
// transcript shifts just enough to split.
r = scenario(200, 500, { scrollTop: 500 });
assert.strictEqual(r.placement, 'split');
assert.strictEqual(r.scrollTop, 564, 'the transcript must scroll by the smallest delta');
insideBand(r);

// Near the header: below, reactions nearest the message.
r = scenario(70, 60, { maxScroll: 0 });
assert.strictEqual(r.placement, 'below');
assert(r.react.top >= r.bubble.bottom && r.actions.top > r.react.top);
insideBand(r);

// Partly under the header with scroll room: revealed, never left clipped.
r = scenario(30, 60, { scrollTop: 400 });
assert(r.bubble.top >= bandTop, 'a clipped message must be scrolled into the band');
insideBand(r);

// Taller than the band: docked above the composer, still clear of the header.
r = scenario(-300, 1400, { maxScroll: 0 });
assert.strictEqual(r.placement, 'dock');
assert.strictEqual(r.actions.top + r.actions.h, bandBottom);
insideBand(r);

// Side alignment: inbound left edges align; outbound right edges align.
r = scenario(300, 60);
assert.strictEqual(r.actions.left, r.bubble.left);
r = scenario(300, 60, { outbound: true });
assert.strictEqual(r.actions.left + r.actions.w, r.bubble.right);
assert(r.react.left >= 12 && r.react.left + r.react.w <= VIEW_W - 12, 'panels stay on screen');

// Side insets (a side status bar, a landscape notch) shrink the usable width.
r = scenario(300, 60, { outbound: true, insets: { top: 0, right: 30, bottom: 0, left: 0 } });
assert(r.react.left + r.react.w <= VIEW_W - 12 - 30 && r.actions.left + r.actions.w <= VIEW_W - 12 - 30,
    'panels stay clear of a right safe-area inset');
r = scenario(300, 60, { insets: { top: 0, right: 0, bottom: 0, left: 40 } });
assert(r.react.left >= 12 + 40 && r.actions.left >= 12 + 40, 'panels stay clear of a left safe-area inset');

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
