// Placement of the message menu: on desktop one kit menu under its anchor,
// above it when there is no room, docked in the transcript band otherwise,
// never over the header or composer, aligned to the message's own side. On a
// phone (RS.kit.placeAround) the reactions sit above the pressed message and
// the actions below it, moving together to one side when the other lacks room.
const assert = require('assert');
const fs = require('fs');
const path = require('path');
const vm = require('vm');

const root = path.resolve(__dirname, '..', '..');
const source = fs.readFileSync(path.join(root, 'dashboard/static/js/lxmf.js'), 'utf8');
const start = source.indexOf('var MSG_ACTION_GAP = 8;');
const end = source.indexOf('function _prepareMessageActionTarget');
assert(start > 0 && end > start, 'placement block must stay extractable');
const kitSource = fs.readFileSync(path.join(root, 'dashboard/static/js/ui_kit.js'), 'utf8');
const kitStart = kitSource.indexOf('    var AT_GAP = 8, AT_EDGE = 12;');
const kitEnd = kitSource.indexOf('    function menu(trigger, opts) {');
assert(kitStart > 0 && kitEnd > kitStart, 'placeAround must stay extractable');
assert(kitSource.includes('kit.placeAround = placeAround;'));

const VIEW_W = 1000;
const VIEW_H = 800;
const TOP_BOTTOM = 64;
const COMPOSER_TOP = 730;
const MENU_H = 200;
const MENU_W = 248;
const REACT_H = 54;
const REACT_W = 330;

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
    const panel = (h, w) => ({ offsetHeight: h, offsetWidth: w, style: { setProperty: function(k, v) { this[k] = v; } } });
    const menuAttrs = {};
    const menu = panel(MENU_H, MENU_W);
    menu.setAttribute = (k, v) => { menuAttrs[k] = v; };
    const style = menu.style;
    const react = panel(REACT_H, REACT_W);
    const actions = panel(MENU_H, MENU_W);
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
        atMessage: !!opts.phone,
        reactPanel: react,
        actionsPanel: actions,
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
    vm.runInContext(kitSource.slice(kitStart, kitEnd), context);
    context.RS = { kit: { placeAround: context.placeAround } };
    const placement = context._layoutMessageActionPanels(state, true);
    const box = (p, h) => ({ top: parseFloat(p.style.top), left: parseFloat(p.style.left), bottom: parseFloat(p.style.top) + h,
        origin: p.style['--ui-at-origin'] });
    return {
        placement: placement,
        attr: menuAttrs['data-placement'],
        menu: { top: parseFloat(style.top), left: parseFloat(style.left) },
        react: box(react, REACT_H),
        actions: box(actions, MENU_H),
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

// Anchored to an element (a delivery mark): under it, not the bubble.
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

// Phone: no sheet. The reactions sit just above the message and the actions just below it.
function insidePhoneBand(r) {
    [r.react, r.actions].forEach(function(p) {
        assert(p.top >= bandTop, 'a panel never covers the header');
        assert(p.bottom <= bandBottom, 'a panel never covers the composer');
    });
}
r = scenario(300, 60, { phone: true, scrollTop: 400 });
assert.strictEqual(r.placement, 'split');
assert.strictEqual(r.attr, 'split');
assert.strictEqual(r.react.bottom, r.bubble.top - 8, 'reactions sit just above the message');
assert.strictEqual(r.actions.top, r.bubble.bottom + 8, 'actions sit just below it');
assert.strictEqual(r.scrollTop, 400, 'a message with room on both sides does not move');
assert.strictEqual(r.react.origin, 'bottom left', 'the reactions grow up from the message');
assert.strictEqual(r.actions.origin, 'top left', 'the actions grow down from it');
insidePhoneBand(r);

// Near the header with room below: both below, reactions nearest the message, nothing moves.
r = scenario(90, 60, { phone: true, scrollTop: 400 });
assert.strictEqual(r.placement, 'below');
assert.strictEqual(r.react.top, r.bubble.bottom + 8);
assert.strictEqual(r.actions.top, r.react.bottom + 8);
assert.strictEqual(r.scrollTop, 400);
insidePhoneBand(r);

// The newest message, already at the end: both above, reactions nearest the message.
r = scenario(600, 60, { phone: true, scrollTop: 400, maxScroll: 400 });
assert.strictEqual(r.placement, 'above');
assert.strictEqual(r.react.bottom, r.bubble.top - 8);
assert.strictEqual(r.actions.bottom, r.react.top - 8);
assert.strictEqual(r.actions.origin, 'bottom left');
insidePhoneBand(r);

// Partly under the composer with history below it: the smallest shift that fits the split.
r = scenario(700, 60, { phone: true, scrollTop: 400 });
assert.strictEqual(r.placement, 'split');
assert.strictEqual(r.actions.bottom, bandBottom, 'the transcript moves only as far as the actions need');
insidePhoneBand(r);

// Partly under the header: it comes down into view.
r = scenario(40, 60, { phone: true, scrollTop: 400 });
assert(r.bubble.top >= bandTop, 'the pressed message is fully in view');
insidePhoneBand(r);

// Taller than the room: docked in the band.
r = scenario(-300, 1400, { phone: true, scrollTop: 400, maxScroll: 400 });
assert.strictEqual(r.placement, 'dock');
assert.strictEqual(r.actions.bottom, bandBottom);
assert.strictEqual(r.react.bottom, r.actions.top - 8);
insidePhoneBand(r);

// Side alignment: inbound left edges align; outbound right edges align, for both panels.
r = scenario(300, 60, { phone: true });
assert.strictEqual(r.react.left, r.bubble.left);
assert.strictEqual(r.actions.left, r.bubble.left);
r = scenario(300, 60, { phone: true, outbound: true });
assert.strictEqual(r.react.left + REACT_W, r.bubble.right);
assert.strictEqual(r.actions.left + MENU_W, r.bubble.right);
assert.strictEqual(r.actions.origin, 'top right');

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

// Channels: a phone's long press shows Reply and Report at the message through the kit menu,
// the same placement and motion as Messages, never a sheet.
(function() {
    const channelsSource = fs.readFileSync(path.join(root, 'dashboard/static/js/channels.js'), 'utf8');
    function fnSource(text, name) {
        const at = text.indexOf('function ' + name + '(');
        assert(at !== -1, name + ' must exist');
        let depth = 0;
        for (let i = text.indexOf('{', at); i < text.length; i++) {
            if (text[i] === '{') depth++;
            if (text[i] === '}' && --depth === 0) return text.slice(at, i + 1);
        }
        throw new Error('unterminated ' + name);
    }
    function El(tag, box) {
        const classes = new Set();
        this.tagName = tag;
        this.className = '';
        this.innerHTML = '';
        this.attrs = {};
        this.children = [];
        this.parentNode = null;
        this.isConnected = true;
        this.offsetWidth = 240;
        this.offsetHeight = 104;
        this.box = box;
        this.style = { setProperty: function(k, v) { this[k] = v; } };
        this.classList = { add: (c) => classes.add(c), remove: (c) => classes.delete(c), contains: (c) => classes.has(c) };
    }
    El.prototype.setAttribute = function(k, v) { this.attrs[k] = String(v); };
    El.prototype.addEventListener = function() {};
    El.prototype.appendChild = function(child) { child.parentNode = this; this.children.push(child); return child; };
    El.prototype.removeChild = function(child) { this.children.splice(this.children.indexOf(child), 1); child.parentNode = null; return child; };
    El.prototype.contains = function(node) { for (; node; node = node.parentNode) if (node === this) return true; return false; };
    El.prototype.getBoundingClientRect = function() { return this.box(); };
    El.prototype.querySelector = function(selector) { return (this.found || {})[selector] || null; };

    const docListeners = {};
    const winListeners = {};
    const transcript = new El('div', () => rect(0, 760, 0, 390));
    transcript.found = { '.ch-room-head': new El('div', () => rect(0, 64, 0, 390)) };
    let articleTop = 300;
    const article = new El('article', () => rect(articleTop, 52, 0, 390));
    article.found = { '.ch-msg-text': new El('p', () => rect(articleTop + 24, 24, 60, 314)) };
    const context = {
        innerWidth: 390, innerHeight: 844, visualViewport: null,
        matchMedia: () => ({ matches: true }),
        addEventListener: (type, fn) => { winListeners[type] = fn; },
        removeEventListener: (type, fn) => { if (winListeners[type] === fn) delete winListeners[type]; },
        safeAreaInsets: () => ({ top: 0, right: 0, bottom: 0, left: 0 }),
        document: {
            body: null,
            activeElement: null,
            createElement: (tag) => new El(tag, () => rect(0, 0, 0, 0)),
            getElementById: (id) => (id === 'channel-transcript' ? transcript : null),
            addEventListener: (type, fn) => { docListeners[type] = fn; },
            removeEventListener: (type, fn) => { if (docListeners[type] === fn) delete docListeners[type]; },
        },
        Math: Math, Object: Object, Array: Array, String: String, Number: Number, Date: Date, Intl: Intl,
    };
    context.window = context;
    vm.createContext(context);
    vm.runInContext(kitSource, context, { filename: 'ui_kit.js' });
    context.document.body = new El('body');
    let fire = null;
    context.RS.gestures = { attachLongPress: (el, opts) => { fire = opts.onFire; } };
    context._channelsCanCompose = () => true;
    context._channelsInsertQuote = () => {};
    context._channelsReportMessage = () => {};
    context._channelsEl = (id) => context.document.getElementById(id);
    vm.runInContext(fnSource(channelsSource, '_channelsBindTouchReplyAction') + '\n' +
        fnSource(channelsSource, '_channelsMessageActionBand'), context);

    context._channelsBindTouchReplyAction(article, { id: 'm1', text: 'hello', ours: false }, 'Ada');
    fire();
    const body = context.document.body;
    assert.strictEqual(body.children.length, 1, 'one panel, no sheet');
    const pop = body.children[0];
    assert.strictEqual(pop.className, 'ui-menu ui-pop ui-pop--at');
    assert(/Reply[\s\S]*ui-menu-item--danger[\s\S]*Report/.test(pop.innerHTML), 'Reply first, Report last');
    assert.strictEqual(pop.style.top, (300 + 52 + 8) + 'px', 'just below the message');
    assert.strictEqual(pop.style.left, '60px', 'lined up with the text, not the avatar');
    assert.strictEqual(pop.style['--ui-at-origin'], 'top left');
    assert(article.classList.contains('is-pressed') && transcript.classList.contains('is-acting'));

    // Scrolled out of the transcript: it closes and the message lets go.
    articleTop = -100;
    winListeners.scroll();
    assert.strictEqual(body.children.length, 0);
    assert(!article.classList.contains('is-pressed') && !transcript.classList.contains('is-acting'));

    // Near the composer: above the message. A tap on the message itself closes it.
    articleTop = 690;
    fire();
    assert.strictEqual(body.children[0].style.top, (690 - 8 - 104) + 'px');
    assert.strictEqual(body.children[0].style['--ui-at-origin'], 'bottom left');
    docListeners.pointerdown({ target: article });
    assert.strictEqual(body.children.length, 0, 'a tap anywhere but the menu closes it');

    // Your own message: Reply only.
    context._channelsBindTouchReplyAction(article, { id: 'm2', text: 'mine', ours: true }, 'You');
    articleTop = 300;
    fire();
    assert(!body.children[0].innerHTML.includes('Report'));
    docListeners.pointerdown({ target: transcript });
}());

console.log('Message action placement tests passed.');
