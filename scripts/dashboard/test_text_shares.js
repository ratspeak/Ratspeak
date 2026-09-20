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
        setAttribute(k, v) { this.attrs[k] = v; }, focus() {}, blur() {},
        querySelector() { return this.children[0] || null; },
        dispatchEvent(event) { for (const fn of this.handlers[event.type] || []) fn(event); },
        async click() { if (this.disabled) return; for (const fn of this.handlers.click || []) await fn({}); }
    };
}
async function settle() { for (let i = 0; i < 35; i++) await Promise.resolve(); }
async function pick(f, index = 0) {
    await f.node('text-share-list').children[index].click();
    return f.node('text-share-confirm').click();
}
function deferred() {
    let resolve, reject;
    const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
    return { promise, resolve, reject };
}
function fixture(android = true, image = false) {
    const nodes = {}, events = {}, timers = new Map(), calls = [], warnings = [], choices = [], sent = [];
    const hooks = {};
    const node = id => nodes[id] ||= element(id);
    const model = { id: A, revision: '1', text: '00123\nПривет https://ozon.example', identity: null, recipient: null, send_attempted: false };
    if (image) { model.image = {name:'photo.jpg',mime:'image/jpeg',size:2_000_000}; model.text = ''; }
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
        attachSharedImage(image, stage) {
            const pending = {name:image.name,preparing:true}; context.lxmfPendingFile = pending;
            pending.stage_promise = stage.then(token => {pending.preparing=false;return token;}).catch(() => {context.lxmfPendingFile=null;return null;});
            return pending;
        },
        sendLxmfMessage(method, holder) { sent.push({ method, holder, text: node('lxmf-input').value }); context.RS.textShares.dispatched(holder); }
    };
    context.window = context;
    context.RS = {
        voiceMemos: { hasPendingRecording: () => false },
        ui: { focusAfterUpdate() {}, openExistingSheet: el => el.classList.add('open'), closeExistingSheet: el => el.classList.remove('open') },
        gestures: { attachDragDismiss(el, options) { hooks.drag = options; } },
        conversationOwner: {
            snapshot: () => ({ identityGeneration: identity, epoch: navigation, hash: context.lxmfActiveContact }),
            isIdentityCurrent: o => o.identityGeneration === identity,
            isCurrent: o => o.identityGeneration === identity && o.epoch === navigation && o.hash === context.lxmfActiveContact
        },
        listen(name, fn) { (events[name] ||= []).push(fn); return Promise.resolve(); },
        invoke(name, args) {
            calls.push({ name, args });
            // Native commits and their IPC replies are separate events. A hook
            // delays only publication, preserving the exact persisted revision.
            const reply = value => hooks.reply ? hooks.reply(name, args, value) : Promise.resolve(value);
            if (name === 'list_text_shares') return reply({ items: pending.map(x => ({...x})), activity_generation: '1', identity_generation: String(identity) });
            if (name === 'api_contacts') return reply(context.lxmfContacts);
            if (name === 'cancel_attachment_stage') return Promise.resolve({cancelled:true});
            assert.equal(name, 'edit_text_share');
            if (failWrite) return Promise.reject(new Error('disk full'));
            const a = args.args, item = pending.find(x => x.id === a.id);
            assert(item); assert.equal(a.revision, item.revision);
            assert.equal(a.identity_generation, String(identity));
            if (a.operation === 'stage_image') return reply({item:{...item},stage:{token:'private-token',...item.image}});
            if (a.operation === 'discard') { pending = pending.filter(x => x !== item); return reply({ item: null }); }
            if (a.operation === 'assign') { item.recipient = a.recipient; item.identity = C; item.send_attempted = false; }
            if (a.text !== undefined) item.text = a.text;
            if (a.operation === 'sending') item.send_attempted = true;
            item.revision = String(++revision);
            return reply({ item: { ...item } });
        }
    };
    node('msg-profile-name').textContent = 'Me';
    vm.runInNewContext(source, context);
    return { context, node, sent, model, warnings, calls, choices, hooks,
        async tick() { const jobs = [...timers.values()]; timers.clear(); jobs.forEach(fn => fn()); await settle(); },
        async event(name) { for (const fn of events[name] || []) fn({}); await settle(); },
        switchIdentity() { identity++; }, failWrites() { failWrite = true; },
        pending: () => pending,
        intake() { pending.push({ ...model, id: B, revision: String(++revision), identity: null, recipient: null }); }
    };
}
(async () => {
    const selection = fixture(); await settle();
    assert(selection.node('text-share-confirm').disabled);
    assert(selection.node('text-share-search').hidden, 'recents have no search/filter chrome');
    assert(selection.node('text-share-back').hidden);
    assert(selection.node('text-share-discard').hidden, 'fresh intake is not crowded by recovery actions');
    await selection.node('text-share-confirm').click();
    await selection.node('text-share-list').children[0].click(); await settle();
    assert.equal(selection.context.lxmfActiveContact, null, 'selection alone must not navigate');
    assert.equal(selection.calls.filter(c => c.name === 'edit_text_share').length, 0, 'selection alone must not mutate native recovery');
    assert.equal(selection.node('text-share-list').children[0].attrs['aria-pressed'], 'true');
    assert(!selection.node('text-share-confirm').disabled);
    await selection.node('text-share-contacts').click();
    assert(selection.node('text-share-confirm').disabled, 'switching lists clears an otherwise invisible selection');
    assert(!selection.node('text-share-search').hidden);
    assert(!selection.node('text-share-back').hidden);
    await selection.node('text-share-list').children[0].click();
    selection.node('text-share-search').value = 'Dad';
    selection.node('text-share-search').dispatchEvent({type:'input'});
    assert(selection.node('text-share-confirm').disabled, 'filtering cannot keep a hidden selected contact');
    assert.equal(selection.node('text-share-list').children.length, 1);
    await selection.node('text-share-back').click();
    assert(selection.node('text-share-search').hidden);
    selection.hooks.drag.onCommit();
    assert(!selection.node('text-share-sheet').classList.contains('open'), 'header swipe dismisses the picker');
    assert.equal(selection.pending().length, 1, 'swipe preserves native recovery');
    await selection.node('text-share-pending').click(); await settle();
    assert(!selection.node('text-share-discard').hidden, 'Shared items review retains explicit discard');
    selection.choices.push('discard'); await selection.node('text-share-discard').click(); await settle();
    assert.equal(selection.pending().length, 0);

    const recent = fixture();
    recent.context.lxmfConversations = Array.from({length:8}, (_,i)=>({hash:(i+1).toString(16).repeat(32),timestamp:i,display_name:'Chat '+i}));
    await settle();
    assert.equal(recent.node('text-share-list').children.length, 5, 'only five recent chats');
    assert.equal(recent.node('text-share-list').children[0].children[1].children[0].textContent, 'Chat 7');
    const oldRecipient = fixture(); oldRecipient.model.recipient = 'd'.repeat(32); await settle();
    await oldRecipient.context.RS.textShares.refresh(true); await settle();
    await oldRecipient.node('text-share-list').children[0].click();
    assert.equal(oldRecipient.context.lxmfActiveContact,null,'opening saved recovery does not navigate');
    assert.equal(oldRecipient.node('text-share-list').children.length,3,'saved recipient outside recents/contacts remains visible');
    assert.equal(oldRecipient.node('text-share-list').children[2].attrs['aria-pressed'],'true');
    assert(!oldRecipient.node('text-share-confirm').disabled);
    const empty = fixture(); empty.context.lxmfConversations = []; await settle();
    assert(!empty.node('text-share-contacts').hidden, 'Contacts remains available with no recent chats');
    assert(empty.node('text-share-confirm').disabled);

    const cancelAssign = fixture(); await settle();
    const assignAck = deferred(); let assignment;
    cancelAssign.hooks.reply = (name,args,value) => {
        if (args?.args?.operation === 'assign') {assignment=value;return assignAck.promise;}
        return Promise.resolve(value);
    };
    const assigning = pick(cancelAssign); await settle();
    assert(cancelAssign.node('text-share-confirm').disabled, 'in-flight Share is disabled');
    await cancelAssign.node('text-share-confirm').click();
    assert.equal(cancelAssign.calls.filter(c=>c.args?.args?.operation==='assign').length,1);
    await cancelAssign.node('text-share-close').click();
    assignAck.resolve(assignment); await assigning; await settle();
    assert.equal(cancelAssign.context.lxmfActiveContact,null,'cancelled assignment cannot reopen the conversation');
    assert.equal(cancelAssign.pending().length,1);

    const photo = fixture(true, true); await settle();
    assert.equal(photo.node('text-share-preview').textContent, 'Photo · photo.jpg');
    await pick(photo); await settle();
    assert.equal(photo.sent.length, 0, 'selecting a photo recipient never sends');
    assert.equal(photo.context.lxmfPendingFile.name, 'photo.jpg');
    assert.equal(photo.node('lxmf-input').value, '', 'photo needs no placeholder caption');
    assert.equal(photo.context.RS.textShares.interceptSend('auto'), true); await settle();
    assert.equal(photo.sent.length, 1, 'photo without caption uses explicit Send');
    photo.context.RS.textShares.accepted(photo.sent[0].holder); await settle();
    assert.equal(photo.pending().length, 0);

    const removedPhoto = fixture(true, true); await settle();
    await pick(removedPhoto); await settle();
    removedPhoto.context.lxmfPendingFile = null;
    assert.equal(removedPhoto.context.RS.textShares.interceptSend('auto'), false, 'removed photo must not trap ordinary composer');
    assert.equal(removedPhoto.pending().length, 1, 'removing staged attachment preserves encrypted recovery');

    for (const transition of ['navigate','identity']) {
        const stalePhoto = fixture(true, true); await settle();
        const ack = deferred(); let staged;
        stalePhoto.hooks.reply = (name, args, value) => {
            if (args?.args?.operation === 'stage_image') { staged=value; return ack.promise; }
            return Promise.resolve(value);
        };
        await pick(stalePhoto); await settle();
        assert(staged);
        assert.equal(stalePhoto.context.RS.textShares.interceptSend('auto'), true);
        assert.equal(stalePhoto.sent.length, 0, 'preparing photo cannot send early');
        if (transition === 'navigate') stalePhoto.context.openConversationWith(C);
        else { stalePhoto.switchIdentity(); await stalePhoto.event('identity_switching'); }
        ack.resolve(staged); await settle();
        assert(stalePhoto.calls.some(call => call.name === 'cancel_attachment_stage'), transition + ': retire late native stage');
        assert.equal(stalePhoto.sent.length, 0);
        assert.equal(stalePhoto.pending().length, 1);
    }
    const unsupported = fixture(false); await settle(); assert.equal(unsupported.calls.length, 0);
    const f = fixture(); await settle();
    assert.equal(f.node('text-share-preview').textContent, f.model.text);
    assert.equal(f.node('text-share-list').children[0].children[1].children[0].textContent, 'Dad', 'most recent first');
    await f.node('text-share-contacts').click();
    assert.equal(f.node('text-share-list').children[0].children[1].children[0].textContent, 'Alice', 'contacts alphabetical');
    await f.node('text-share-back').click();
    await pick(f); await settle();
    assert.equal(f.context.lxmfActiveContact, B); assert.equal(f.node('lxmf-input').value, f.model.text);
    assert.equal(f.sent.length, 0, 'recipient selection does not send');
    f.node('lxmf-input').value += '\nEdited'; f.node('lxmf-input').dispatchEvent({ type: 'input' }); await f.tick();
    assert(f.model.text.endsWith('Edited'), 'edits persist in the recovery record');
    assert(f.context.RS.textShares.interceptSend('auto')); await settle();
    assert.equal(f.sent.length, 1); assert(f.model.send_attempted, 'record send ambiguity before transport');
    f.context.RS.textShares.accepted(f.sent[0].holder); await settle(); assert.equal(f.pending().length, 0);

    const conflict = fixture(); conflict.context._lxmfDrafts[B] = 'Existing draft'; await settle();
    conflict.choices.push('later'); await pick(conflict); await settle();
    assert.equal(conflict.context.lxmfActiveContact, null); assert.equal(conflict.context._lxmfDrafts[B], 'Existing draft');
    conflict.choices.push('add'); await pick(conflict); await settle();
    assert(conflict.node('lxmf-input').value.startsWith('Existing draft\n\n00123'));

    const media = fixture(); await settle(); media.context.RS.voiceMemos.hasPendingRecording = () => true;
    await pick(media); assert.equal(media.context.lxmfActiveContact, null);
    assert(media.warnings.some(x => x.includes('Finish your voice'))); assert.equal(media.sent.length, 0);

    const disk = fixture(); await settle(); disk.failWrites();
    await pick(disk); await settle();
    assert.equal(disk.context.lxmfActiveContact, null); assert.equal(disk.pending().length, 1);

    const stale = fixture(); await settle(); const row = stale.node('text-share-list').children[0];
    stale.switchIdentity(); await stale.event('identity_switching'); await row.click(); await settle();
    assert.equal(stale.context.lxmfActiveContact, null); assert.equal(stale.sent.length, 0);

    const recovered = fixture(); recovered.model.recipient = B; recovered.model.identity = C; recovered.model.send_attempted = true;
    await settle(); await recovered.context.RS.textShares.refresh(true); await settle();
    recovered.choices.push('view'); await pick(recovered); await settle();
    assert.equal(recovered.context.lxmfActiveContact, B); assert.equal(recovered.node('lxmf-input').value, '');
    assert.equal(recovered.sent.length, 0, 'recovery never resends automatically');
    assert.equal(recovered.pending().length, 1, 'viewing does not consume a recovery copy');

    const warm = fixture(); await settle();
    await warm.node('text-share-close').click();
    warm.intake(); await warm.event('text_shares_available');
    assert(warm.node('text-share-sheet').classList.contains('open'), 'a new warm share opens its picker after an earlier share was deferred');

    // Qualify async ownership against the production controller, not a second
    // implementation of the guards. Replies can arrive after native storage
    // committed, so interruption must keep recovery without pasting or sending.
    for (const transition of ['navigate', 'identity', 'hidden', 'pagehide', 'voice', 'file', 'reply', 'draft']) {
        const delayed = fixture(); await settle();
        const ack = deferred(); let committed;
        delayed.hooks.reply = (name, args, value) => {
            if (name === 'edit_text_share' && args.args.operation === 'assign') {
                committed = value; return ack.promise;
            }
            return Promise.resolve(value);
        };
        const selecting = pick(delayed); await settle();
        assert(committed, transition + ': assignment reached native storage before interruption');
        if (transition === 'navigate') delayed.context.openConversationWith(C);
        if (transition === 'identity') { delayed.switchIdentity(); await delayed.event('identity_switching'); }
        if (transition === 'hidden') { delayed.context.document.visibilityState = 'hidden'; await delayed.event('visibilitychange'); }
        if (transition === 'pagehide') await delayed.event('pagehide');
        if (transition === 'voice') delayed.context.RS.voiceMemos.hasPendingRecording = () => true;
        if (transition === 'file') delayed.context.lxmfPendingFile = { name: 'attachment' };
        if (transition === 'reply') delayed.context._replyTarget = { id: 'reply' };
        if (transition === 'draft') delayed.context._lxmfDrafts[B] = 'Typed while native storage was saving';
        ack.resolve(committed); await selecting; await settle();
        assert.equal(delayed.context.lxmfActiveContact, transition === 'navigate' ? C : null,
            transition + ': a retired assignment reply must not navigate');
        assert.equal(delayed.node('lxmf-input').value, '', transition + ': no late paste');
        assert.equal(delayed.context._lxmfDrafts[B], transition === 'draft' ? 'Typed while native storage was saving' : undefined,
            transition + ': preserve a replacement text draft');
        assert.equal(delayed.pending().length, 1, transition + ': native recovery survives');
        assert.equal(delayed.sent.length, 0, transition + ': no implicit send');
    }

    {
        const overlap = fixture(); await settle();
        const listed = deferred(); let oldList, listCalls = 0;
        overlap.hooks.reply = (name, _args, value) => {
            if (name === 'list_text_shares' && ++listCalls === 1) { oldList = value; return listed.promise; }
            return Promise.resolve(value);
        };
        const refresh = overlap.context.RS.textShares.refresh(true);
        await pick(overlap); await settle();
        assert.equal(overlap.model.recipient, B);
        const noticesBeforeReply = [...overlap.warnings];
        assert.equal(oldList.items[0].recipient, null, 'the delayed inventory predates assignment');
        listed.resolve(oldList); await refresh; await settle();
        assert.equal(listCalls, 2, 'inventory overlapping a mutation must be read again');
        assert.equal(overlap.node('text-share-sheet').classList.contains('open'), false,
            'a stale unassigned inventory must not reopen the picker after selection');
        assert.equal(overlap.node('lxmf-input').value, overlap.model.text);
        assert.equal(overlap.context.RS.textShares.interceptSend('auto'), true);
        await settle();
        assert.equal(overlap.sent.length, 1, 'the selected holder retains its current native revision');
        assert.deepEqual(overlap.warnings, noticesBeforeReply, 'no stale-revision storage error');
    }

    for (const transition of ['navigate', 'identity', 'hidden', 'voice', 'file', 'reply', 'draft']) {
        const interrupted = fixture(); await settle();
        await pick(interrupted); await settle();
        const ack = deferred(); let committed;
        interrupted.hooks.reply = (name, args, value) => {
            if (name === 'edit_text_share' && args.args.operation === 'sending') {
                committed = value; return ack.promise;
            }
            return Promise.resolve(value);
        };
        assert.equal(interrupted.context.RS.textShares.interceptSend('auto'), true);
        await settle();
        assert(committed, transition + ': sending marker committed before reply');
        assert.equal(interrupted.context.RS.textShares.interceptSend('auto'), true);
        assert.equal(interrupted.calls.filter(call => call.args?.args.operation === 'sending').length, 1,
            'a second click cannot duplicate an in-flight send admission');
        if (transition === 'navigate') interrupted.context.openConversationWith(C);
        if (transition === 'identity') { interrupted.switchIdentity(); await interrupted.event('identity_switching'); }
        if (transition === 'hidden') { interrupted.context.document.visibilityState = 'hidden'; await interrupted.event('visibilitychange'); }
        if (transition === 'voice') interrupted.context.RS.voiceMemos.hasPendingRecording = () => true;
        if (transition === 'file') interrupted.context.lxmfPendingFile = { name: 'attachment' };
        if (transition === 'reply') interrupted.context._replyTarget = { id: 'reply' };
        if (transition === 'draft') interrupted.node('lxmf-input').value = 'Newly typed text';
        const input = interrupted.node('lxmf-input').value;
        ack.resolve(committed); await settle();
        assert.equal(interrupted.sent.length, 0, transition + ': delayed admission must not dispatch from a retired composer');
        assert.equal(interrupted.node('lxmf-input').value, input, transition + ': late admission must not erase new text');
        assert.equal(interrupted.pending().length, 1);
        assert.equal(interrupted.model.send_attempted, true, 'an admitted marker remains conservative; never auto-resend');
    }

    {
        const oldSend = fixture(); await settle();
        await pick(oldSend); await settle();
        oldSend.context.RS.textShares.interceptSend('auto'); await settle();
        assert.equal(oldSend.sent.length, 1);
        const holder = oldSend.sent[0].holder;
        oldSend.switchIdentity(); await oldSend.event('identity_switching');
        const callsBefore = oldSend.calls.length;
        oldSend.context.RS.textShares.accepted(holder); await settle();
        assert.equal(oldSend.calls.length, callsBefore, 'acceptance for an old identity cannot mutate the replacement identity store');
        assert.equal(oldSend.pending().length, 1);
        assert.equal(oldSend.node('text-share-pending').hidden, true, 'late acceptance must not repopulate reset UI');
    }

    // Execute the actual normal send adapter, not only this fixture's sender.
    const lxmf = fs.readFileSync(path.join(root, 'dashboard/static/js/lxmf.js'), 'utf8');
    const sendSource = lxmf.slice(lxmf.indexOf('function sendLxmfMessage('), lxmf.indexOf('function sendLxmfVoiceMemo('));
    for (const attachment of [false,true]) for (const response of [{ msg_id: 'queued' }, { cancelled: true }, { msg_id: 'cancelled', cancelled: true }, null]) {
        const results = [];
        const ctx = {
            lxmfActiveContact: B, lxmfPendingFile: attachment ? {destination:B,stage_promise:Promise.resolve('token'),name:'photo.jpg',inline_image:true} : null, _replyTarget: null, lxmfLimits: {},
            _canonicalConversationHash: x => x, _conversationOwnerIsCurrent: () => true,
            _conversationOwnerIdentityIsCurrent: () => true, clearPendingFile: () => {},
            _markOptimisticMessageFailed: () => {},
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
    console.log('Text shares: platform gating, ordering, explicit send, recovery, delayed assignment/send, refresh revision and identity/media/lifecycle fences passed');
})().catch(error => { console.error(error); process.exitCode = 1; });
