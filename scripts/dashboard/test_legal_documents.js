const assert = require('assert');
const fs = require('fs');
const path = require('path');
const vm = require('vm');

const dashboard = path.resolve(__dirname, '..', '..', 'dashboard');
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
let openedSubject = '';
let openedBody = '';
let dragDismiss = null;
const context = {
    console,
    Promise,
    setTimeout,
    window: {
        RS: {
            openExternalUrl(url) { openedUrl = url; return Promise.resolve(true); },
            openSupportEmail(subject, body) {
                openedSubject = subject;
                openedBody = body;
                return Promise.resolve(true);
            },
            gestures: {
                attachDragDismiss(element, options) {
                    dragDismiss = { element, options };
                    return {};
                }
            }
        }
    },
    document: { createElement(tag) { return new FakeElement(tag); } },
    _rsBuildSheet(options) {
        builtSheet = {
            options,
            overlay: new FakeElement('overlay'),
            sheet: new FakeElement('sheet'),
            title: new FakeElement('h2'),
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
assert.strictEqual(legal.version, '2026-11-08');
assert.strictEqual(
    Array.from(Object.keys(legal.documents)).join(','),
    'privacy,terms,guidelines,support,licenses'
);
const canonicalHeadings = {
    privacy: [
        'Privacy at a glance', 'Who and what this policy covers',
        'Information stored on your device', 'Direct messages, media, and calls',
        'Propagation nodes and relays', 'Public and shared channels',
        'Website, downloads, and support', 'Device permissions',
        'Retention, deletion, and security', 'Your choices and controls',
        'Questions or requests'
    ],
    terms: [
        'Your agreement with Ratspeak', 'Eligibility', 'Beta and pre-release software',
        'A decentralized, independently operated network', 'Public and shared channels',
        'Acceptable use', 'Your content and responsibility',
        'Reports, blocking, and enforcement', 'Changes and availability',
        'Open-source components', 'Service limits', 'Changes and contact'
    ],
    guidelines: [
        'Where these guidelines apply', 'Treat people as people',
        'Content and conduct we do not allow', 'Be a good network participant',
        'Public channels and independent hubs', 'Block, leave, and report',
        'How Ratspeak responds', 'Immediate danger and support'
    ]
};
Object.keys(canonicalHeadings).forEach((documentId) => {
    canonicalHeadings[documentId].forEach((heading) => {
        assert(
            legal.documents[documentId].content.includes('<h2>' + heading + '</h2>'),
            documentId + ' offline copy is missing current section: ' + heading
        );
    });
});
// Frontend components ship their license texts beside them and in the bundle's resources.
const tauriConfig = JSON.parse(fs.readFileSync(path.join(dashboard, '..', 'src-tauri', 'tauri.conf.json'), 'utf8'));
[
    ['jsQR 1.4.0', 'static/js/vendor/jsQR.LICENSE.txt', 'third-party/jsQR/LICENSE.txt', 'Apache License'],
    ['Outfit', 'static/fonts/Outfit.LICENSE.txt', 'third-party/fonts/Outfit.LICENSE.txt', 'Copyright 2021 The Outfit Project Authors'],
    ['JetBrains Mono', 'static/fonts/JetBrainsMono.LICENSE.txt', 'third-party/fonts/JetBrainsMono.LICENSE.txt', 'Copyright 2020 The JetBrains Mono Project Authors']
].forEach(([name, file, resource, marker]) => {
    assert(legal.documents.licenses.content.includes('<li>' + name + ', '), 'Licenses must list ' + name);
    assert(fs.readFileSync(path.join(dashboard, file), 'utf8').includes(marker), file + ' must carry its license text');
    assert.strictEqual(tauriConfig.bundle.resources['../dashboard/' + file], resource, name + ' license must ship in the bundle');
});
['Outfit', 'JetBrainsMono'].forEach((family) => {
    assert(fs.readFileSync(path.join(dashboard, 'static/fonts', family + '.LICENSE.txt'), 'utf8')
        .includes('SIL OPEN FONT LICENSE Version 1.1'), family + ' must ship the OFL text');
});
assert.strictEqual(index.includes('/static/js/legal_documents.js'), true);
assert(css.includes('[data-legal-document-id="licenses"] .rs-legal-content section'), 'Licenses set each component in a well');
assert(!css.includes('.rs-legal-eyebrow'), 'the dialog title names the document; no uppercase eyebrow');

assert.strictEqual(legal.open('privacy'), true);
assert.strictEqual(builtSheet.wasPresented, true);
assert.strictEqual(builtSheet.options.size, 'lg');
assert.strictEqual(builtSheet.options.sheetClass, 'rs-legal-sheet');
assert.strictEqual(builtSheet.title.textContent, 'Privacy Policy');
assert.strictEqual(builtSheet.body.children.length, 1);
assert.strictEqual(dragDismiss.element, builtSheet.sheet);
assert.strictEqual(dragDismiss.options.handleSelector, '.ui-dialog-head');
assert.strictEqual(dragDismiss.options.blockIfScrolled, false);
assert(builtSheet.body.children[0].innerHTML.includes('Available offline'));
assert(builtSheet.body.children[0].innerHTML.includes('Ratspeak does not currently operate a public channel hub.'));
assert(builtSheet.body.children[0].innerHTML.includes('Vercel Web Analytics'));
assert(builtSheet.body.children[0].innerHTML.includes('is not linked to your Ratspeak identity'));
assert(builtSheet.body.children[0].innerHTML.includes('connecting IP address'));
assert(builtSheet.body.children[0].innerHTML.includes('Age eligibility'));

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
assert.strictEqual(builtSheet.title.textContent, 'Community Guidelines');
assert.strictEqual(article.attributes['data-legal-document-id'], 'guidelines');
assert(article.innerHTML.includes('Open networks still need human boundaries.'));
assert(article.innerHTML.includes('Violence and targeted harm'));
assert(article.innerHTML.includes('Abuse and exploitation'));
assert(!article.innerHTML.includes('Child sexual abuse and exploitation'));
assert.strictEqual(builtSheet.body.scrollTop, 0);

builtSheet.footer.children[0].listeners.click();
article.listeners.click({
    preventDefault() {},
    target: {
        closest(selector) {
            if (selector === '[data-legal-email]') {
                return { getAttribute() { return 'Ratspeak safety report'; } };
            }
            return null;
        }
    }
});
dragDismiss.options.onCommit();
assert.strictEqual(builtSheet.wasDismissed, true);
setImmediate(() => {
    assert.strictEqual(openedUrl, 'https://ratspeak.org/community-guidelines.html');
    assert.strictEqual(openedSubject, 'Ratspeak safety report');
    assert.strictEqual(openedBody, '');
    console.log('Offline legal document tests passed');
});
