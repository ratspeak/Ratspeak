// Bulk message selection: only the tray's Select row selects; taps anywhere on
// a message's row toggle; Cancel or leaving the conversation ends it; the bar
// reads "Delete" then "Delete (N)"; in-flight sends are never selectable; and
// confirmed deletes go through delete_messages.
const assert = require('assert');
const fs = require('fs');
const path = require('path');
const vm = require('vm');

const root = path.resolve(__dirname, '..', '..');
const source = fs.readFileSync(path.join(root, 'dashboard/static/js/lxmf.js'), 'utf8');
const html = fs.readFileSync(path.join(root, 'dashboard/index.html'), 'utf8');
const start = source.indexOf('var _msgSelection = null;');
const end = source.indexOf('function _messageActionOwnsText');
assert(start > 0 && end > start, 'selection block must stay extractable');

function classList() {
    const set = new Set();
    return {
        has: (c) => set.has(c),
        toggle: (c, on) => { if (on) set.add(c); else set.delete(c); },
        add: (c) => set.add(c),
        remove: (c) => set.delete(c),
    };
}

function harness(messages) {
    const bar = { hidden: true, offsetHeight: 44, contains: () => false };
    const deleteBtn = { textContent: 'Delete', attrs: {}, setAttribute(k, v) { this.attrs[k] = v; } };
    const status = { textContent: '' };
    const container = { classList: classList(), querySelectorAll: () => [], contains: () => true, scrollTop: 100 };
    const calls = [];
    const confirms = [];
    const toasts = [];
    const context = {
        lxmfConversation: messages.slice(),
        lxmfActiveContact: 'peer',
        lxmfActiveThread: '',
        _msgReactions: {},
        _canonicalConversationHash: (h) => h,
        _messageCanCancelSend: (m) => m.direction === 'outbound' && m.state === 'sending',
        _messageDisplayContent: (m) => m.content,
        _conversationNameInfo: () => ({ name: 'Marisol', isHash: false }),
        _dismissContextMenu: () => false,
        _consumePendingMessageHoldActivation: () => false,
        _focusMessageControl: () => {},
        isCompactLayout: () => true,
        renderConversation: () => {},
        cacheGet: () => null,
        cacheSet: () => {},
        haptic: () => {},
        showToast: (text, kind) => toasts.push([text, kind]),
        setTimeout: (fn) => fn(),
        clearTimeout: () => {},
        rsConfirm: (opts) => { confirms.push(opts); return Promise.resolve(true); },
        RS: { invoke: (cmd, args) => { calls.push([cmd, args]); return Promise.resolve({ deleted: args.ids.length }); } },
        document: {
            activeElement: null,
            getElementById: (id) => ({ 'lxmf-messages': container, 'lxmf-select-bar': bar,
                'lxmf-select-delete-btn': deleteBtn, 'lxmf-select-status': status })[id] || null,
        },
    };
    vm.createContext(context);
    ['_canonicalConversationThread', '_conversationKey', '_activeConversationKey', '_resolveConversationThread'].forEach((name) => {
        const at = source.indexOf('function ' + name + '(');
        vm.runInContext(source.slice(at, source.indexOf('\n}\n', at) + 3), context);
    });
    vm.runInContext(source.slice(start, end), context);
    return { context, bar, deleteBtn, status, container, calls, confirms, toasts };
}

const msg = (id, extra) => Object.assign({ id: id, direction: 'inbound', state: 'received', content: id }, extra || {});
const messages = [msg('a'), msg('b'), msg('c'), msg('out_x', { direction: 'outbound', state: 'sending' }),
    msg('d', { image: { stored_name: 'img' } })];
const settle = () => new Promise((r) => setImmediate(r));

(async function() {
    let h = harness(messages);
    const c = h.context;
    c._selectMessage(messages[0]);
    assert.strictEqual(c._messageSelectionActive(), true);
    assert.strictEqual(h.bar.hidden, false, 'the selection bar appears');
    assert.strictEqual(h.container.scrollTop, 144, 'the transcript stays still while the bar takes its height');
    assert.strictEqual(h.deleteBtn.textContent, 'Delete', 'one selected reads plain Delete');
    assert.strictEqual(h.status.textContent, '1 selected');
    assert(h.container.classList.has('msg-select-mode'));

    c._toggleMessageSelection(messages[1]);
    c._toggleMessageSelection(messages[2]);
    assert.strictEqual(h.deleteBtn.textContent, 'Delete (3)');
    assert.strictEqual(h.status.textContent, '3 selected');
    c._toggleMessageSelection(messages[3]);
    assert.strictEqual(h.deleteBtn.textContent, 'Delete (3)', 'an in-flight send is never selectable');
    c._selectMessage(messages[3]);
    assert.strictEqual(h.deleteBtn.textContent, 'Delete (3)');

    c._toggleMessageSelection(messages[1]);
    c._toggleMessageSelection(messages[2]);
    c._toggleMessageSelection(messages[0]);
    assert.strictEqual(c._messageSelectionActive(), false, 'deselecting the last message exits');
    assert.strictEqual(h.bar.hidden, true);
    assert.strictEqual(h.container.scrollTop, 100);
    assert.strictEqual(h.status.textContent, 'Selection cleared');

    // The gaps between messages do nothing; leaving the conversation (header,
    // conversation list, navigation) ends selection, except the back arrow,
    // which ends it on its own, and never while the confirm is open.
    const inside = (surface) => ({ closest: (sel) => (sel === '.msg-row' ? null : surface) });
    c._selectMessage(messages[0]);
    h.container.contains = () => true;
    c._handleMessageSelectionClick({ target: inside(null) });
    assert.strictEqual(c._messageSelectionActive(), true, 'a gap between messages keeps selection');
    h.container.contains = () => false;
    c._handleMessageSelectionClick({ target: { closest: (sel) => (sel === '#lxmf-back-btn' ? {} : sel.includes('#lxmf-chat-header') ? {} : null) } });
    assert.strictEqual(c._messageSelectionActive(), true, 'the back arrow handles its own exit');
    c._handleMessageSelectionClick({ target: { closest: () => null } });
    assert.strictEqual(c._messageSelectionActive(), true, 'composer and other chat chrome keep selection');
    c._msgSelection.pending = true;
    c._handleMessageSelectionClick({ target: { closest: (sel) => (sel.includes('.lxmf-sidebar') ? {} : null) } });
    assert.strictEqual(c._messageSelectionActive(), true, 'the confirm sheet never ends selection');
    c._msgSelection.pending = false;
    c._handleMessageSelectionClick({ target: { closest: (sel) => (sel.includes('.lxmf-sidebar') ? {} : null) } });
    assert.strictEqual(c._messageSelectionActive(), false, 'the conversation list ends selection');
    h.container.contains = () => true;

    // A tap anywhere on another message's row toggles it instead of running its controls.
    c._selectMessage(messages[0]);
    let stopped = false;
    const bubbleB = { getAttribute: () => 'b' };
    c._handleMessageSelectionClick({
        target: { closest: () => ({ querySelector: () => bubbleB }) },
        preventDefault() {}, stopPropagation() { stopped = true; },
    });
    assert(stopped);
    assert.strictEqual(h.deleteBtn.textContent, 'Delete (2)');
    assert.strictEqual(c._clearMessageSelection(), true);
    assert.strictEqual(c._clearMessageSelection(), false);

    // Selection belongs to one conversation.
    c._selectMessage(messages[0]);
    c.lxmfActiveContact = 'other';
    assert.strictEqual(c._messageSelectionActive(), false);
    c.lxmfActiveContact = 'peer';
    c._clearMessageSelection();

    // Confirmed delete: local-only copy, media line, count on the action.
    c._selectMessage(messages[1]);
    c._toggleMessageSelection(messages[4]);
    c._deleteSelectedMessages();
    await settle();
    await settle();
    assert.strictEqual(h.confirms.length, 1);
    assert.strictEqual(h.confirms[0].title, 'Delete 2 messages?');
    assert.strictEqual(h.confirms[0].confirmText, 'Delete 2');
    assert.strictEqual(h.confirms[0].danger, true);
    assert(h.confirms[0].message.includes('removed from this device only. Marisol still has their copy.'));
    assert(h.confirms[0].message.includes('Photos and files in these messages'));
    assert.deepStrictEqual(JSON.parse(JSON.stringify(h.calls)), [['delete_messages', { hash: 'peer', ids: ['b', 'd'] }]]);
    assert.deepStrictEqual(c.lxmfConversation.map((m) => m.id), ['a', 'c', 'out_x']);
    assert.strictEqual(c._messageSelectionActive(), false);

    // Rows deleted but a file left behind is still a delete, not a failure.
    h = harness(messages);
    h.context.RS.invoke = () => Promise.reject(Object.assign(new Error('files'), { code: 'files_not_removed' }));
    h.context._selectMessage(messages[0]);
    h.context._deleteSelectedMessages();
    await settle();
    await settle();
    assert.deepStrictEqual(h.toasts, [['Messages deleted. Some files could not be removed.', 'toast-warning']]);
    assert.deepStrictEqual(h.context.lxmfConversation.map((m) => m.id), ['b', 'c', 'out_x', 'd']);
    assert.strictEqual(h.context._messageSelectionActive(), false);

    // Cancelled confirm deletes nothing and keeps the selection.
    h = harness(messages);
    h.context.rsConfirm = (opts) => { h.confirms.push(opts); return Promise.resolve(false); };
    h.context._selectMessage(messages[0]);
    h.context._deleteSelectedMessages();
    await settle();
    assert.strictEqual(h.confirms[0].title, 'Delete message?');
    assert.strictEqual(h.confirms[0].confirmText, 'Delete');
    assert.strictEqual(h.calls.length, 0);
    assert.strictEqual(h.context._messageSelectionActive(), true);

    // Wiring: opening actions never selects or draws a circle; Select sits
    // between Reply and Copy; Esc, Back, the back arrow and Cancel exit.
    const open = source.slice(source.indexOf('function _showMsgContextMenu('), source.indexOf('function _handleMessageActionPointer('));
    assert(!open.includes('_selectMessage(msgData);\n    window') && !source.includes('is-preview'));
    const tray = source.slice(source.indexOf("replyBtn.setAttribute('data-message-action', 'reply')"),
        source.indexOf("copyBtn.setAttribute('data-message-action', 'copy-message')"));
    assert(tray.includes("selectBtn.setAttribute('data-message-action', 'select')") &&
        tray.includes("(isSelected ? 'Deselect' : 'Select')"), 'Select must sit between Reply and Copy');
    assert(source.includes("document.addEventListener('click', _handleMessageSelectionClick, true);"));
    assert(/_dismissContextMenu\(\) \|\|\s*\(!\(_msgSelection && _msgSelection\.pending\) && _clearMessageSelection\(\)\)/.test(source));
    assert(source.includes('return _clearMessageSelection();\n};'), 'native Back exits selection');
    assert(source.includes('if (_clearMessageSelection()) return;\n            RS.viewStack.pop();'));
    assert(source.includes("selectCancelBtn.addEventListener('click', _clearMessageSelection);"));
    assert(html.includes('<button class="lxmf-select-bar-btn" id="lxmf-select-cancel-btn" type="button">Cancel</button>'));
    assert(html.includes('id="lxmf-select-delete-btn" type="button">Delete</button>'));
    assert(html.indexOf('id="lxmf-select-bar"') > html.indexOf('id="chat-header-menu-btn"'),
        'the bar sits under the header; Call and More stay in place');

    console.log('Message selection tests passed.');
})().catch((err) => { console.error(err); process.exit(1); });
