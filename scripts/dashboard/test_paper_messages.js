#!/usr/bin/env node
// Paper messages: error decoding, the setting, the decoder's clipboard rules,
// the shared result view, the native lxm:// handoff, and the send flow.
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const js = path.join(__dirname, '..', '..', 'dashboard', 'static', 'js');
const read = name => fs.readFileSync(path.join(js, name), 'utf8');
const paperSource = read('paper_messages.js');
const bridgeSource = read('native_paper_link.js');
const lxmfSource = read('lxmf.js');
const settingsSource = read('settings.js');
const contactSource = read('contact_card.js');
const settle = () => new Promise(resolve => setTimeout(resolve, 0));
const wait = ms => new Promise(resolve => setTimeout(resolve, ms));

function fn(source, name) {
    const start = source.indexOf('function ' + name + '(');
    assert(start >= 0, name);
    let depth = 0;
    for (let i = source.indexOf('{', start); i < source.length; i++) {
        if (source[i] === '{') depth++;
        if (source[i] === '}' && --depth === 0) return source.slice(start, i + 1);
    }
    throw new Error('unterminated ' + name);
}

class El {
    constructor(tag) {
        this.tagName = String(tag || 'div').toUpperCase();
        this.children = [];
        this.attributes = {};
        this.listeners = {};
        this.hidden = false;
        this.disabled = false;
        this.textContent = '';
        this.value = '';
        this.style = {};
        this._classes = new Set();
        this._queries = {};
        this._html = '';
        const self = this;
        this.classList = {
            add: (...names) => names.forEach(n => self._classes.add(n)),
            remove: (...names) => names.forEach(n => self._classes.delete(n)),
            contains: n => self._classes.has(n),
            toggle: (n, on) => (on === undefined ? !self._classes.has(n) : on) ? self._classes.add(n) : self._classes.delete(n),
        };
    }
    set className(value) { this._classes = new Set(String(value).split(/\s+/).filter(Boolean)); }
    get className() { return Array.from(this._classes).join(' '); }
    set innerHTML(value) { this._html = String(value); this._queries = {}; }
    get innerHTML() { return this._html; }
    appendChild(child) { this.children.push(child); child.parentNode = this; return child; }
    setAttribute(name, value) { this.attributes[name] = String(value); }
    getAttribute(name) { return this.attributes[name]; }
    removeAttribute(name) { delete this.attributes[name]; }
    addEventListener(type, handler) { (this.listeners[type] = this.listeners[type] || []).push(handler); }
    dispatch(type, event) { (this.listeners[type] || []).forEach(handler => handler(event || {})); }
    click() { if (!this.disabled) this.dispatch('click', {}); }
    focus() { documentStub.activeElement = this; }
    querySelector(selector) {
        if (!this._html) return null;
        return this._queries[selector] || (this._queries[selector] = new El('div'));
    }
}

const documentStub = {
    activeElement: null,
    documentElement: { dataset: {} },
    createElement: tag => new El(tag),
};

function paperContext(platform, clipboardText) {
    const invokes = [];
    const toasts = [];
    const sheets = [];
    const reveals = [];
    const clipboardReads = [];
    const responses = {};
    const context = {
        document: documentStub,
        navigator: {
            clipboard: {
                readText() {
                    clipboardReads.push(Date.now());
                    return clipboardText instanceof Error ? Promise.reject(clipboardText) : Promise.resolve(clipboardText);
                }
            }
        },
        setTimeout, clearTimeout, Promise, JSON, RegExp, Number, String, Object, Array, Math, Date,
        isIOS: () => platform === 'ios',
        escapeHtml: value => String(value).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;'),
        identityAvatar: hash => '<svg data-hash="' + hash + '"></svg>',
        shortHash: hash => String(hash).slice(0, 8) + '…' + String(hash).slice(-4),
        _lookupContactRecord: () => null,
        _conversationNameInfo: hash => ({ name: 'Announced ' + String(hash).slice(0, 4) }),
        _formatDateLabel: () => 'Today',
        formatTime: () => '09:41',
        showToast: (message, kind) => toasts.push([message, kind]),
        openConversationAtMessage: (...args) => reveals.push(args),
        _channelsBuildLinkMethod: (title, detail) => {
            const button = new El('button');
            button.title = title;
            button.detail = detail;
            return button;
        },
        _channelsSheetField: (label, input) => {
            const field = new El('div');
            field.label = label;
            field.appendChild(input);
            return field;
        },
        _channelsPresentSheet: (built, focus) => { built.presentedFocus = focus; },
        _rsBuildSheet: (opts, onClose) => {
            const built = { opts, body: new El('div'), footer: new El('div'), sheet: new El('div'), dismissed: 0 };
            built.dismiss = value => { built.dismissed++; onClose(value); };
            sheets.push(built);
            return built;
        },
    };
    context.window = context;
    context.RS = {
        qr: { openScanner: () => invokes.push(['scanner']) },
        ui: { focusAfterUpdate: el => { context.focused = el; } },
        invoke(command, args) {
            invokes.push([command, args]);
            const response = responses[command];
            if (response instanceof Error) return Promise.reject(response);
            return Promise.resolve(typeof response === 'function' ? response(args) : response);
        },
    };
    vm.createContext(context);
    vm.runInContext(paperSource, context, { filename: 'paper_messages.js' });
    return { context, invokes, toasts, sheets, reveals, clipboardReads, responses };
}

function textOf(sheet) {
    return sheet.body.children.map(child => [child.textContent].concat(child.children.map(c => c.textContent || c.title || '')).join(' ')).join(' ');
}

const URI = 'lxm://AbC_d-9__';
const PREVIEW = {
    status: 'ok', message_id: 'm-1', source_hash: 'ab'.repeat(16), display_name: 'Field Rat',
    timestamp: 1730000000, title: '', content: '**Meet** at noon', signature: 'verified', blocked: false,
};

(async function() {
    // Error codes cross either a `code` field or the message text.
    const { context: ctx } = paperContext('desktop', '');
    const coded = new Error('too long'); coded.code = 'paper_too_long'; coded.overage_chars = 42;
    assert.equal(ctx.paperErrorCode(coded), 'paper_too_long');
    assert.equal(ctx.paperErrorDetail(coded, 'overage_chars'), 42);
    // The backend shape: { code, message, details: { overage_chars } } through RS.invoke.
    const stateSource = read('state.js');
    const invokeContext = {
        window: {
            __TAURI_INTERNALS__: { invoke: () => Promise.reject({ code: 'paper_too_long', message: 'Too long', details: { overage_chars: 9 } }) },
            RS: {},
        },
        setInterval, clearInterval, Promise, Error,
    };
    vm.runInNewContext(fn(stateSource, '_rsInvokeAvailable') + fn(stateSource, '_rsWaitForInvoke') +
        stateSource.slice(stateSource.indexOf('window.RS.invoke = function'), stateSource.indexOf('// Copy text to the clipboard')), invokeContext);
    const bridged = await invokeContext.window.RS.invoke('create_paper_message', {}).catch(e => e);
    assert.equal(ctx.paperErrorCode(bridged), 'paper_too_long');
    assert.equal(ctx.paperErrorDetail(bridged, 'overage_chars'), 9, 'RS.invoke keeps error details');
    const jsonError = new Error('{"code":"paper_too_long","overage_chars":1284}'); jsonError.code = 'error';
    assert.equal(ctx.paperErrorCode(jsonError), 'paper_too_long');
    assert.equal(ctx.paperErrorDetail(jsonError, 'overage_chars'), 1284);
    const textError = new Error('paper_too_long: over by 77 characters'); textError.code = 'bad_request';
    assert.equal(ctx.paperErrorCode(textError), 'paper_too_long');
    assert.equal(ctx.paperErrorDetail(textError, 'overage_chars'), 77);
    assert.equal(ctx.paperErrorCode('paper_not_for_you'), 'paper_not_for_you');
    const locked = new Error('locked'); locked.code = 'locked';
    assert.equal(ctx.paperErrorCode(locked), 'locked');
    assert.equal(ctx.RS.paper.errorText(new Error('paper_decrypt_failed')),
        'Couldn\'t decrypt this paper message. It may be incomplete or damaged.');
    assert.equal(ctx.RS.paper.errorText({ code: 'paper_not_for_you' }), 'This paper message isn\'t addressed to you.');
    assert.equal(ctx.RS.paper.errorText(new Error('paper_invalid')), 'That isn\'t a valid paper message.');

    // Desktop and Android read the clipboard exactly once, on open.
    for (const platform of ['desktop', 'android']) {
        const t = paperContext(platform, '  ' + URI + '  ');
        t.responses.preview_paper_message = PREVIEW;
        t.context.openPaperDecoder();
        const built = t.sheets[0];
        assert.equal(built.opts.title, 'Open paper message');
        const [form] = built.body.children;
        const [copy, tools] = form.children;
        assert.equal(copy.textContent, 'Paste a paper link or scan its QR.');
        const [paste, scan] = tools.children;
        assert.deepEqual([paste.title, paste.detail], ['Paste message from clipboard', 'A paper link is on your clipboard']);
        assert.deepEqual([scan.title, scan.detail], ['Scan QR', 'Use the camera or choose an image']);
        assert.equal(paste.hidden, true, 'the paste row waits for a matching clipboard');
        await wait(300);
        assert.equal(t.clipboardReads.length, 1);
        assert.equal(paste.hidden, false);
        assert.strictEqual(t.context.focused, paste, 'initial focus is the paste row when shown');
        paste.click();
        await settle();
        assert.equal(t.clipboardReads.length, 1, 'tapping reuses the single read');
        assert.deepEqual(JSON.parse(JSON.stringify(t.invokes[0])), ['preview_paper_message', { uri: URI }]);
        await settle();
        assert.equal(form.hidden, true, 'the result replaces the form');
        assert.equal(built.footer.hidden, true);
        const result = built.body.children[1];
        assert(result.innerHTML.includes('Verified sender'));
    }
    const noMatch = paperContext('android', 'https://example.com');
    noMatch.context.openPaperDecoder();
    await wait(300);
    const noMatchTools = noMatch.sheets[0].body.children[0].children[1];
    assert.equal(noMatchTools.children[0].hidden, true, 'non-paper clipboard text keeps the row hidden');
    assert.strictEqual(noMatch.context.focused.tagName, 'TEXTAREA', 'focus falls back to the field');
    assert.equal(noMatch.clipboardReads.length, 1);

    // iOS never reads on open; the Paste row is always shown and reads on tap.
    const ios = paperContext('ios', URI);
    ios.responses.preview_paper_message = PREVIEW;
    ios.context.openPaperDecoder();
    const iosPaste = ios.sheets[0].body.children[0].children[1].children[0];
    assert.deepEqual([iosPaste.title, iosPaste.detail, iosPaste.hidden], ['Paste', 'Paste a copied paper link', false]);
    await wait(300);
    assert.equal(ios.clipboardReads.length, 0);
    iosPaste.click();
    await settle(); await settle();
    assert.equal(ios.clipboardReads.length, 1);
    assert.equal(ios.invokes[0][0], 'preview_paper_message');

    // The field: 4096 chars, mono, no autocorrect; pasting lxm:// reviews at once.
    const pasted = paperContext('ios', '');
    pasted.responses.preview_paper_message = new Error('paper_not_for_you');
    pasted.context.openPaperDecoder();
    const field = pasted.sheets[0].body.children[0].children[2];
    const input = field.children[0];
    assert.equal(field.label, 'Paper link');
    assert.deepEqual([input.maxLength, input.rows, input.placeholder], [4096, 4, 'lxm://…']);
    assert.deepEqual([input.attributes.autocorrect, input.attributes.autocapitalize, input.attributes.spellcheck, input.attributes.writingsuggestions],
        ['off', 'none', 'false', 'false']);
    input.value = URI;
    input.dispatch('paste');
    await settle(); await settle(); await settle();
    const errorLine = pasted.sheets[0].body.children[0].children[3];
    assert.equal(errorLine.attributes['aria-live'], 'polite');
    assert.equal(errorLine.textContent, 'This paper message isn\'t addressed to you.');
    const [cancel, review] = pasted.sheets[0].footer.children;
    assert.deepEqual([cancel.textContent, review.textContent, review.disabled], ['Cancel', 'Review', false]);

    // A chat or native link opens straight on the result, with no clipboard read.
    const linked = paperContext('desktop', URI);
    linked.responses.preview_paper_message = Object.assign({}, PREVIEW, { status: 'duplicate', signature: 'unknown' });
    let closedWith = null;
    linked.context.openPaperDecoder({ uri: URI, autoReview: true, onClose: how => { closedWith = how; } });
    assert.equal(linked.sheets[0].body.children[0].hidden, true);
    await settle(); await settle();
    assert.equal(linked.clipboardReads.length, 0);
    const linkedResult = linked.sheets[0].body.children[1];
    assert(linkedResult.innerHTML.includes('Unverified sender'));
    assert(linkedResult.innerHTML.includes('Their identity isn&#39;t known yet') || linkedResult.innerHTML.includes('Their identity isn\'t known yet'));
    assert(linkedResult.innerHTML.includes('Already in this chat'));
    assert(linkedResult.innerHTML.includes('>Open chat</button>'));
    linkedResult.querySelector('.paper-result-add').click();
    assert.equal(closedWith, 'open');
    await wait(260);
    assert.deepEqual(JSON.parse(JSON.stringify(linked.reveals)), [[PREVIEW.source_hash, 'm-1', PREVIEW.timestamp]]);
    assert(!linked.invokes.some(call => call[0] === 'add_paper_message'), 'a known duplicate only opens the chat');

    // Result view: Markdown stripped into a scrollable preview; Add files then reveals.
    const added = paperContext('desktop', '');
    added.context.RS.markdown = { strip: text => String(text).replace(/\*\*/g, '') };
    added.responses.add_paper_message = { status: 'duplicate', message_id: 'm-9', source_hash: PREVIEW.source_hash, timestamp: 5 };
    const parent = new El('div');
    let closes = 0;
    added.context.RS.paper.renderResult(parent, URI, PREVIEW, { close: () => closes++ });
    assert(parent.innerHTML.includes('<div class="contact-scan-name">Field Rat</div>'));
    assert(parent.innerHTML.includes('<span>Sent</span><code class="paper-result-plain">Today, 09:41</code>'));
    assert(parent.innerHTML.includes('<span>From</span><code>abababab…abab</code>'));
    assert(parent.innerHTML.includes('tabindex="0" aria-label="Message preview"'));
    assert.equal(parent.querySelector('.paper-result-preview').textContent, 'Meet at noon');
    const addButton = parent.querySelector('.paper-result-add');
    addButton.click();
    assert.equal(addButton.textContent, 'Adding…');
    await settle(); await settle();
    assert.deepEqual(JSON.parse(JSON.stringify(added.invokes.at(-1))), ['add_paper_message', { uri: URI }]);
    assert.deepEqual(added.toasts, [['Already in this chat', 'toast-info']], 'a duplicate found on add is announced');
    assert.equal(closes, 1);
    await wait(260);
    assert.deepEqual(JSON.parse(JSON.stringify(added.reveals)), [[PREVIEW.source_hash, 'm-9', 5]]);

    // Invalid signatures and blocked senders cannot be added.
    for (const variant of [{ signature: 'invalid' }, { blocked: true }]) {
        const blockedParent = new El('div');
        added.context.RS.paper.renderResult(blockedParent, URI, Object.assign({}, PREVIEW, variant), {});
        assert(blockedParent.innerHTML.includes('class="nr-btn nr-btn-primary paper-result-add" type="button" disabled>Add to chat'));
        assert(variant.blocked
            ? blockedParent.innerHTML.includes('You blocked this sender.')
            : blockedParent.innerHTML.includes('Signature doesn&#39;t match') || blockedParent.innerHTML.includes('Signature doesn\'t match'));
    }

    // Settings: OFF by default, mirrored to data-paper-messages, wired to the backend.
    const handlers = { off: {}, on: {} };
    const radio = (name, checked) => ({ checked, disabled: false, closest: () => ({ setAttribute() {} }),
        addEventListener: (type, handler) => { handlers[name][type] = handler; } });
    const off = radio('off', true), on = radio('on', false);
    const settingsCalls = [];
    const settingsContext = {
        window: {},
        document: { documentElement: { dataset: {} }, getElementById: id => ({ 'settings-paper-messages-off': off, 'settings-paper-messages-on': on })[id] || null },
        RS: { invoke: (command, args) => { settingsCalls.push([command, args]); return Promise.resolve({ enabled: args.enabled }); } },
    };
    const settingsStart = settingsSource.indexOf('var _settingsPaperMessagesBound');
    const settingsEnd = settingsSource.indexOf('window.ratspeakPaperMessagesEnabled');
    vm.runInNewContext(settingsSource.slice(settingsStart, settingsEnd) +
        fn(settingsSource, 'applyAppSettingsPayload').replace(/^function applyAppSettingsPayload\(data\) \{[\s\S]*?(    if \(data\.paper_messages_enabled)/, 'function applyAppSettingsPayload(data) {\n$1').replace(/\n    var lxmfLimitToggle[\s\S]*$/, '\n}') +
        '\nthis.init = initPaperMessagesToggle; this.apply = applyAppSettingsPayload;', settingsContext);
    settingsContext.init();
    assert.equal(settingsContext.document.documentElement.dataset.paperMessages, 'off');
    on.checked = true; off.checked = false;
    handlers.on.change();
    await settle(); await settle();
    assert.deepEqual(JSON.parse(JSON.stringify(settingsCalls)), [['set_paper_messages_enabled', { enabled: true }]]);
    assert.equal(settingsContext.document.documentElement.dataset.paperMessages, 'on');
    settingsContext.apply({ paper_messages_enabled: false });
    assert.equal(settingsContext.document.documentElement.dataset.paperMessages, 'off');
    assert.deepEqual([off.checked, on.checked], [true, false]);
    const indexHtml = fs.readFileSync(path.join(js, '..', '..', 'index.html'), 'utf8');
    const spam = indexHtml.indexOf('id="settings-hide-known-spam-peers-on"');
    const paperRow = indexHtml.indexOf('<span class="settings-row-label">Paper messages</span>');
    const blockList = indexHtml.indexOf('<span class="settings-row-label">Block List</span>');
    assert(spam < paperRow && paperRow < blockList, 'the row sits between spam peers and the block list');
    assert(indexHtml.includes('Open messages shared as a paper QR or link. Adds Paper to the new message menu.'));
    assert(indexHtml.includes('id="settings-paper-messages-off" value="off" checked'));
    assert(settingsSource.includes("if (data.paper_messages_enabled !== undefined) {\n        adoptPaperMessagesFromBackend(data.paper_messages_enabled);"));

    // Native handoff: peek on start, open the decoder prefilled, ack the exact link on close.
    const bridgeCalls = [];
    let decoderOpts = null;
    const bridgeContext = {
        window: { __RATSPEAK_MOBILE__: true, openPaperDecoder: opts => { decoderOpts = opts; return {}; } },
        document: { visibilityState: 'visible', body: {}, querySelector: () => null, addEventListener() {} },
        RS: {
            diag() {},
            listen: () => Promise.resolve(() => {}),
            invoke(command, args) {
                bridgeCalls.push([command, args]);
                return Promise.resolve(command === 'peek_native_paper_link' ? { uri: URI } : true);
            },
        },
        setTimeout, Promise,
    };
    bridgeContext.window.RS = bridgeContext.RS;
    vm.runInNewContext(bridgeSource, bridgeContext);
    await wait(20);
    assert.equal(bridgeCalls[0][0], 'peek_native_paper_link');
    assert(decoderOpts && decoderOpts.uri === URI && decoderOpts.autoReview === true);
    assert(!bridgeCalls.some(call => call[0] === 'ack_native_paper_link'), 'no ack before the user acts');
    decoderOpts.onClose('dismiss');
    await settle();
    assert.deepEqual(JSON.parse(JSON.stringify(bridgeCalls.find(call => call[0] === 'ack_native_paper_link'))),
        ['ack_native_paper_link', { uri: URI }]);
    assert(bridgeSource.includes("RS.listen('native_paper_link_available', signal, { required: true })"));

    // Send flow: attachment, overage, and unknown-identity warnings leave the composer alone.
    async function send(setup) {
        const toastsSent = [], shares = [], appended = [];
        const input = { value: '  Meet at noon  ', style: {} };
        const sendContext = {
            lxmfActiveContact: 'cd'.repeat(16),
            lxmfPendingFile: null,
            _replyTarget: null,
            _paperSendInFlight: false,
            document: { getElementById: () => input },
            showToast: (message, kind, ms) => toastsSent.push([message, kind, ms]),
            _conversationOwnerSnapshot: () => ({ hash: 'cd'.repeat(16) }),
            _conversationOwnerIdentityIsCurrent: () => true,
            _conversationOwnerIsCurrent: () => true,
            _canonicalConversationHash: hash => hash,
            _canonicalConversationThread: thread => String(thread || ''),
            _conversationNameInfo: () => ({ name: 'Field Rat' }),
            _conversationMessagesFor: () => ({ messages: [], active: true }),
            _appendConversationMessage: (hash, message) => { appended.push(message); return true; },
            _prepareComposedMessage: text => ({ preview: text }),
            _updateConversationPreview() {},
            _finishLxmfComposerSend: target => { target.value = ''; },
            clearReplyTarget() {},
            renderConversation() {},
            loadConversations() {},
            generateMsgId: () => 'client-1',
            paperErrorCode: ctx.paperErrorCode,
            paperErrorDetail: ctx.paperErrorDetail,
            RS: { qr: { openPaperShare: opts => shares.push(opts) }, invoke: () => Promise.resolve() },
            Promise, Object, Number, Date, Error,
        };
        setup(sendContext);
        vm.runInNewContext(fn(lxmfSource, 'sendLxmfPaperMessage') + '\nthis.send = sendLxmfPaperMessage;', sendContext);
        sendContext.send();
        await settle(); await settle(); await settle();
        return { toastsSent, shares, appended, input };
    }
    const attachment = await send(c => { c.lxmfPendingFile = {}; });
    assert.deepEqual(attachment.toastsSent, [['Paper messages can\'t include attachments.', 'toast-warning', 4000]]);
    const tooLong = await send(c => {
        c.RS.invoke = () => { const e = new Error('Too long'); e.code = 'paper_too_long'; e.details = { overage_chars: 1284 }; return Promise.reject(e); };
    });
    assert.deepEqual(tooLong.toastsSent, [['Too long for a paper message. Remove about 1,284 characters.', 'toast-warning', 4000]]);
    assert.equal(tooLong.input.value, '  Meet at noon  ', 'warnings leave the composer untouched');
    const unknown = await send(c => {
        c.RS.invoke = () => Promise.reject(new Error('paper_unknown_identity'));
    });
    assert.deepEqual(unknown.toastsSent, [['Can\'t create a paper message until Field Rat announces.', 'toast-warning', 4000]]);
    let created = null;
    const sent = await send(c => {
        c.RS.invoke = (command, args) => {
            created = [command, args];
            return Promise.resolve({ message: { id: 'm-2', timestamp: 7 }, paper_uri: URI, uri_chars: URI.length });
        };
    });
    assert.deepEqual(JSON.parse(JSON.stringify(created)), ['create_paper_message', { dest_hash: 'cd'.repeat(16), content: 'Meet at noon', title: '', thread: null }]);
    assert.equal(sent.input.value, '', 'success clears the composer');
    assert.deepEqual([sent.appended[0].id, sent.appended[0].state, sent.appended[0].delivery_method, sent.appended[0].paper_uri],
        ['m-2', 'paper', 'paper', URI]);
    assert.deepEqual([sent.shares[0].uri, sent.shares[0].recipientName], [URI, 'Field Rat'], 'the popup opens on success');
    assert(!('renderer' in sent.appended[0]), 'the sender\'s bubble is plain text, like the receiver\'s');
    assert(contactSource.includes('\'<button class="nr-btn nr-btn-primary" id="contact-scan-add">Add</button>\''),
        'the contact scan result uses the primary action too');
    const viewsCss = fs.readFileSync(path.join(js, '..', 'css', '10-views.css'), 'utf8');
    assert(/\.paper-share-sheet \.paper-share-copy\s*\{[^}]*padding:\s*0;/s.test(viewsCss) &&
        /\.paper-share-sheet \.paper-share-copy svg\s*\{[^}]*width:\s*18px;/s.test(viewsCss),
        'the copy glyph outranks the mobile .nr-btn padding');
    assert(/\.paper-decoder-result \.contact-scan-actions\s*\{[^}]*calc\(var\(--space-4\) \+ var\(--sab\)\)/s.test(viewsCss));
    assert(/\.paper-decoder-sheet \.channel-share-input\s*\{\s*resize:\s*none;/s.test(viewsCss));
    assert(/\.contact-share-sheet\.paper-share-sheet\s*\{[^}]*880px\)/s.test(viewsCss));

    // Scanner: lxm:// payloads route to the paper preview; new default copy.
    assert(contactSource.includes("var scannerTitle = options.title || 'Scan QR Code';"));
    assert(contactSource.includes("var checkingText = options.checkingText || 'Checking QR\\u2026';"));
    assert(contactSource.includes("var invalidText = options.invalidText || 'That QR isn\\'t a contact card, paper message, or lxma:// address.';"));
    assert(contactSource.includes("'No QR code found in that image.'"));
    assert(contactSource.includes("if (/^lxm:\\/\\//i.test(paperUri) && window.RS.paper) {\n                return RS.paper.preview(paperUri).then(function(preview) {\n                    stopStream();\n                    RS.paper.renderResult(body, paperUri, preview, { close: closeAll });"));
    // Popup copy.
    for (const copy of ['Preparing paper message&hellip;', "'<div class=\"contact-share-subtitle\">Ratspeak paper message</div>'",
        "'Only ' + name + ' can read this.'", "Paper link · ' + chars + ' characters</div>", 'aria-label="Copy paper link"',
        "'Paper message QR for ' + name", "title: 'Ratspeak paper message'", "showToast('QR image saved'",
        "showToast('QR code shared'", "copyText(uri, 'Paper link')", "'ratspeak-paper-' + safeFileBase(name)"]) {
        assert(contactSource.includes(copy), 'paper popup: ' + copy);
    }

    console.log('paper messages: errors, decoder, result view, setting, native link, send flow passed');
})().catch(error => { console.error(error); process.exitCode = 1; });
