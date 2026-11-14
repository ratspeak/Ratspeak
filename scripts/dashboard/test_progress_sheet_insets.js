// Run the real progress state machine: loading has no actions, so the footer stays hidden until
// an error or a cancel needs one; the kit dialog owns the home-indicator inset either way.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');
const root = path.join(__dirname, '../..');
const source = fs.readFileSync(path.join(root, 'dashboard/static/js/dialogs.js'), 'utf8');
function element() {
    const classes = new Set();
    return {children: [], style: {}, classList: {add: c => classes.add(c), remove: c => classes.delete(c), contains: c => classes.has(c)},
        appendChild(child) { this.children.push(child); }, replaceChild(next, prev) { this.children[this.children.indexOf(prev)] = next; },
        get firstChild() { return this.children[0]; }, setAttribute() {}, addEventListener() {}, focus() {} };
}
let built;
const context = {document: {activeElement: null, createElement: element}, setTimeout: () => 1, clearTimeout() {},
    RS: {kit: {icon: name => '<svg data-icon="' + name + '"></svg>'}}};
vm.createContext(context);
vm.runInContext(source, context);
context._rsBuildSheet = () => (built = {sheet: element(), body: element(), footer: element(), present() {}, dismiss() {}});
const loading = context.rsProgress({message: 'Starting Bluetooth…'});
assert.equal(built.footer.hidden, true);
loading.success('Ready');
assert.equal(built.footer.hidden, true, 'success stays without actions');
assert.match(built.body.children[0].children[0].innerHTML, /check-circle/);
loading.error('Could not start');
assert.equal(built.footer.hidden, false, 'an error shows Close');
assert.match(built.body.children[0].children[0].innerHTML, /x-circle/);
context.rsProgress({onCancel() {}});
assert.equal(built.footer.hidden, false, 'a cancellable operation shows Cancel');
const kit = fs.readFileSync(path.join(root, 'dashboard/static/css/07-ui-components.css'), 'utf8');
assert.match(kit, /\.ui-dialog, \.ui-dialog--md, \.ui-dialog--lg \{[^}]*padding-bottom: var\(--sab, 0px\);/,
    'the phone sheet keeps clear of the home indicator');
console.log('progress sheet safe-area states passed');
