const assert = require('assert');
const fs = require('fs');
const path = require('path');
const vm = require('vm');

const dashboard = path.resolve(__dirname, '..');
const source = fs.readFileSync(path.join(dashboard, 'static/js/legal_documents.js'), 'utf8');
const css = fs.readFileSync(path.join(dashboard, 'static/css/08-modals.css'), 'utf8');
const index = fs.readFileSync(path.join(dashboard, 'index.html'), 'utf8');

class FakeElement {
    constructor(tag) {
        this.tagName = tag;
        this.children = [];
        this.listeners = {};
        this.attributes = {};
        this.className = '';
        this.innerHTML = '';
        this.scrollTop = -1;
        this.classList = { add: (...names) => { this.addedClasses = names; } };
    }
    appendChild(child) { this.children.push(child); return child; }
    addEventListener(name, callback) { this.listeners[name] = callback; }
    setAttribute(name, value) { this.attributes[name] = value; }
    querySelector() { return null; }
}

let builtSheet = null;
let openedUrl = '';
const context = {
    console,
    Promise,
    setTimeout,
    window: {
        RS: {
            openExternalUrl(url) { openedUrl = url; return Promise.resolve(true); },
            openSupportEmail() { return Promise.resolve(true); }
        }
    },
    document: { createElement(tag) { return new FakeElement(tag); } },
    _rsBuildSheet(options) {
        builtSheet = {
            options,
            overlay: new FakeElement('overlay'),
            sheet: new FakeElement('sheet'),
            body: new FakeElement('body'),
            footer: new FakeElement('footer'),
            present() { this.wasPresented = true; },
            dismiss() { this.wasDismissed = true; }
        };
        return builtSheet;
    }
};
context.RS = context.window.RS;
vm.runInNewContext(source, context, { filename: 'legal_documents.js' });

const legal = context.window.RS.legal;
assert.strictEqual(legal.version, '2026-08-07');
assert.strictEqual(
    Array.from(Object.keys(legal.documents)).join(','),
    'privacy,terms,guidelines,support'
);
assert.strictEqual(index.includes('/static/js/legal_documents.js'), true);
assert(css.includes('.rs-legal-sheet .bottom-sheet-body'));
assert(css.includes('.bottom-sheet.open.rs-legal-sheet'));

assert.strictEqual(legal.open('privacy'), true);
assert.strictEqual(builtSheet.wasPresented, true);
assert.strictEqual(builtSheet.options.showTitle, false);
assert.strictEqual(builtSheet.sheet.attributes['aria-label'], 'Privacy Policy');
assert.strictEqual(builtSheet.body.children.length, 1);
assert(builtSheet.body.children[0].innerHTML.includes('Available offline'));
assert(builtSheet.body.children[0].innerHTML.includes('Ratspeak does not currently operate a public channel hub.'));

const article = builtSheet.body.children[0];
article.listeners.click({
    preventDefault() {},
    target: {
        closest(selector) {
            if (selector === '[data-legal-document]') {
                return { getAttribute() { return 'guidelines'; } };
            }
            return null;
        }
    }
});
assert.strictEqual(builtSheet.sheet.attributes['aria-label'], 'Community Guidelines');
assert(article.innerHTML.includes('Open networks still need human boundaries.'));
assert.strictEqual(builtSheet.body.scrollTop, 0);

builtSheet.footer.children[0].listeners.click();
setImmediate(() => {
    assert.strictEqual(openedUrl, 'https://ratspeak.org/community-guidelines.html');
    console.log('Offline legal document tests passed');
});
