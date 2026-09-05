const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const root = path.resolve(__dirname, '../..');
const source = fs.readFileSync(path.join(root, 'dashboard/static/js/text_shares.js'), 'utf8');
const A = 'a'.repeat(32), B = 'b'.repeat(32), C = 'c'.repeat(32);
function element(id) {
    const classes = new Set();
    return { id, value: '', textContent: '', hidden: false, children: [], handlers: {}, attrs: {}, style: {},
        classList: { add: x => classes.add(x), remove: x => classes.delete(x), contains: x => classes.has(x) },
        addEventListener(type, fn) { (this.handlers[type] ||= []).push(fn); },
        appendChild(node) { this.children.push(node); }, replaceChildren() { this.children = []; },
        setAttribute(k, v) { this.attrs[k] = v; }, focus() {},
        querySelector() { return this.children[0] || null; },
        dispatchEvent(event) { for (const fn of this.handlers[event.type] || []) fn(event); },
        async click() { for (const fn of this.handlers.click || []) await fn({}); }
    };
}
async function settle() { for (let i = 0; i < 35; i++) await Promise.resolve(); }
function fixture(android = true) {
    const nodes = {}, events = {}, timers = new Map(), calls = [], warnings = [], choices = [], sent = [];
    const node = id => nodes[id] ||= element(id);
    const model = { id: A, revision: '1', text: '00123\nПривет https://ozon.example', identity: null, recipient: null, send_attempted: false };
    let pending = [model], revision = 1, identity = 0, navigation = 0, failWrite = false;
    const context = { console, Promise, Object, String, Number, Error, RegExp,
        Event: function(type) { this.type = type; },
        setTimeout(fn) { const key = {}; timers.set(key, fn); return key; }, clearTimeout(key) { timers.delete(key); },
        isAndroid: () => android, _isSetupActive: () => false,
        document: { visibilityState: 'visible', getElementById: node, createElement: tag => element(tag),
            querySelector: () => null, addEventListener(type, fn) { (events[type] ||= []).push(fn); } },
        addEventListener(type, fn) { (events[type] ||= []).push(fn); },
        lxmfPendingFile: null, _replyTarget: null, lxmfActiveContact: null, _lxmfDrafts: {},
        lxmfContacts: [{ hash: B, display_name: 'Dad' }, { hash: C, display_name: 'Alice' }],
        lxmfConversations: [{ hash: B, display_name: 'Dad', timestamp: 20 }, { hash: C, display_name: 'Alice', timestamp: 10 }],
        normalizeContactList: x => x, shortHash: x => x.slice(0, 8), identityAvatar: () => '<svg></svg>',
        showToast: text => warnings.push(text), rsChoice: async () => choices.shift() || null,
        openConversationWith(hash) { navigation++; context.lxmfActiveContact = hash; node('lxmf-input').value = context._lxmfDrafts[hash] || ''; },
        sendLxmfMessage(method, holder) { sent.push({ method, holder, text: node('lxmf-input').value }); context.RS.textShares.dispatched(holder); }
    };
    context.window = context;
    context.RS = {
        voiceMemos: { hasPendingRecording: () => false },
        ui: { openExistingSheet: el => el.classList.add('open'), closeExistingSheet: el => el.classList.remove('open') },
        conversationOwner: {
            snapshot: () => ({ identityGeneration: identity, epoch: navigation, hash: context.lxmfActiveContact }),
            isIdentityCurrent: o => o.identityGeneration === identity,
            isCurrent: o => o.identityGeneration === identity && o.epoch === navigation && o.hash === context.lxmfActiveContact
        },
        listen(name, fn) { (events[name] ||= []).push(fn); return Promise.resolve(); },
        invoke(name, args) {
            calls.push({ name, args });
            if (name === 'list_text_shares') return Promise.resolve({ items: pending.map(x => ({...x})), activity_generation: '1', identity_generation: String(identity) });
            if (name === 'api_contacts') return Promise.resolve(context.lxmfContacts);
            assert.equal(name, 'edit_text_share');
            if (failWrite) return Promise.reject(new Error('disk full'));
            const a = args.args, item = pending.find(x => x.id === a.id);
            assert(item); assert.equal(a.revision, item.revision);
            assert.equal(a.identity_generation, String(identity));
            if (a.operation === 'discard') { pending = pending.filter(x => x !== item); return Promise.resolve({ item: null }); }
            if (a.operation === 'assign') { item.recipient = a.recipient; item.identity = C; item.send_attempted = false; }
            if (a.text !== undefined) item.text = a.text;
            if (a.operation === 'sending') item.send_attempted = true;
            item.revision = String(++revision);
            return Promise.resolve({ item: { ...item } });
        }
    };
    node('msg-profile-name').textContent = 'Me';
    vm.runInNewContext(source, context);
    return { context, node, sent, model, warnings, calls, choices,
        async tick() { const jobs = [...timers.values()]; timers.clear(); jobs.forEach(fn => fn()); await settle(); },
        async event(name) { for (const fn of events[name] || []) fn({}); await settle(); },
        switchIdentity() { identity++; }, failWrites() { failWrite = true; },
        pending: () => pending,
        intake() { pending.push({ ...model, id: B, revision: String(++revision), identity: null, recipient: null }); }
    };
}
(async () => {
    const unsupported = fixture(false); await settle(); assert.equal(unsupported.calls.length, 0);
    const f = fixture(); await settle();
    assert.equal(f.node('text-share-preview').textContent, f.model.text);
    assert.equal(f.node('text-share-list').children[0].children[1].children[0].textContent, 'Dad', 'most recent first');
    await f.node('text-share-contacts').click();
    assert.equal(f.node('text-share-list').children[0].children[1].children[0].textContent, 'Alice', 'contacts alphabetical');
    await f.node('text-share-recent').click();
    await f.node('text-share-list').children[0].click(); await settle();
    assert.equal(f.context.lxmfActiveContact, B); assert.equal(f.node('lxmf-input').value, f.model.text);
    assert.equal(f.sent.length, 0, 'recipient selection does not send');
    f.node('lxmf-input').value += '\nEdited'; f.node('lxmf-input').dispatchEvent({ type: 'input' }); await f.tick();
    assert(f.model.text.endsWith('Edited'), 'edits persist in the recovery record');
    assert(f.context.RS.textShares.interceptSend('auto')); await settle();
    assert.equal(f.sent.length, 1); assert(f.model.send_attempted, 'record send ambiguity before transport');
    f.context.RS.textShares.accepted(f.sent[0].holder); await settle(); assert.equal(f.pending().length, 0);

    const conflict = fixture(); conflict.context._lxmfDrafts[B] = 'Existing draft'; await settle();
    conflict.choices.push('later'); await conflict.node('text-share-list').children[0].click(); await settle();
    assert.equal(conflict.context.lxmfActiveContact, null); assert.equal(conflict.context._lxmfDrafts[B], 'Existing draft');
    conflict.choices.push('add'); await conflict.node('text-share-list').children[0].click(); await settle();
    assert(conflict.node('lxmf-input').value.startsWith('Existing draft\n\n00123'));

    const media = fixture(); await settle(); media.context.RS.voiceMemos.hasPendingRecording = () => true;
    await media.node('text-share-list').children[0].click(); assert.equal(media.context.lxmfActiveContact, null);
    assert(media.warnings.some(x => x.includes('Finish your voice'))); assert.equal(media.sent.length, 0);

    const disk = fixture(); await settle(); disk.failWrites();
    await disk.node('text-share-list').children[0].click(); await settle();
    assert.equal(disk.context.lxmfActiveContact, null); assert.equal(disk.pending().length, 1);

    const stale = fixture(); await settle(); const row = stale.node('text-share-list').children[0];
    stale.switchIdentity(); await stale.event('identity_switching'); await row.click(); await settle();
    assert.equal(stale.context.lxmfActiveContact, null); assert.equal(stale.sent.length, 0);

    const recovered = fixture(); recovered.model.recipient = B; recovered.model.identity = C; recovered.model.send_attempted = true;
    await settle(); await recovered.context.RS.textShares.refresh(true); await settle();
    recovered.choices.push('view'); await recovered.node('text-share-list').children[0].click(); await settle();
    assert.equal(recovered.context.lxmfActiveContact, B); assert.equal(recovered.node('lxmf-input').value, '');
    assert.equal(recovered.sent.length, 0, 'recovery never resends automatically');
    assert.equal(recovered.pending().length, 1, 'viewing does not consume a recovery copy');

    const warm = fixture(); await settle();
    await warm.node('text-share-close').click();
    warm.intake(); await warm.event('text_shares_available');
    assert(warm.node('text-share-sheet').classList.contains('open'), 'a new warm share opens its picker after an earlier share was deferred');

    // Execute the actual normal send adapter, not only this fixture's sender.
    const lxmf = fs.readFileSync(path.join(root, 'dashboard/static/js/lxmf.js'), 'utf8');
    const sendSource = lxmf.slice(lxmf.indexOf('function sendLxmfMessage('), lxmf.indexOf('function sendLxmfVoiceMemo('));
    for (const response of [{ msg_id: 'queued' }, { cancelled: true }, { msg_id: 'cancelled', cancelled: true }, null]) {
        const results = [];
        const ctx = {
            lxmfActiveContact: B, lxmfPendingFile: null, _replyTarget: null, lxmfLimits: {},
            document: { getElementById: () => ({ value: '00123' }) },
            _conversationOwnerSnapshot: () => ({ hash: B }), _consumeLxmfSendFocusState: () => false,
            _deliveryPrefOrAuto: () => 'auto', _utf8ByteLength: x => x.length, generateMsgId: () => 'client-id',
            _handleLxmfSendAccepted: () => {}, _appendConversationMessage: () => false,
            _optimisticDeliveryMethod: () => 'auto', _updateConversationPreview: () => {},
            loadConversations: () => {}, _finishLxmfComposerSend: () => {},
            RS: { invoke: () => Promise.resolve(response), textShares: {
                dispatched: () => results.push('dispatched'), accepted: () => results.push('accepted'),
                failed: () => results.push('failed')
            } }
        };
        vm.runInNewContext(sendSource, ctx);
        ctx.sendLxmfMessage('auto', {});
        await settle();
        assert.deepEqual(results, ['dispatched', response?.msg_id && !response.cancelled ? 'accepted' : 'failed']);
    }
    console.log('Text shares: platform gating, ordering, explicit send, draft merge, recovery, storage and identity/media fences passed');
})().catch(error => { console.error(error); process.exitCode = 1; });
