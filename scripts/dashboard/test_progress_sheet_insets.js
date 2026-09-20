// Run the real progress state machine; loading has no footer to own safe area.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');
const root = path.join(__dirname, '../..');
const source = fs.readFileSync(path.join(root, 'dashboard/static/js/dialogs.js'), 'utf8');
function element() {
    const classes = new Set();
    return {children: [], style: {}, classList: {add: c => classes.add(c), remove: c => classes.delete(c), contains: c => classes.has(c)},
        appendChild(child) { this.children.push(child); }, setAttribute() {}, addEventListener() {}, focus() {} };
}
let built;
const context = {document: {activeElement: null, createElement: element}, setTimeout: () => 1, clearTimeout() {}};
vm.createContext(context);
vm.runInContext(source, context);
context._rsBuildSheet = () => (built = {sheet: element(), body: element(), footer: element(), present() {}, dismiss() {}});
const loading = context.rsProgress({message: 'Starting Bluetooth…'});
assert.equal(built.footer.hidden, true);
assert(built.sheet.classList.contains('rs-dialog-sheet--footerless'));
loading.success('Ready');
assert(built.sheet.classList.contains('rs-dialog-sheet--footerless'), 'success remains footerless');
loading.error('Could not start');
assert.equal(built.footer.hidden, false);
assert(!built.sheet.classList.contains('rs-dialog-sheet--footerless'), 'error footer resumes safe-area ownership');
context.rsProgress({onCancel() {}});
assert.equal(built.footer.hidden, false);
assert(!built.sheet.classList.contains('rs-dialog-sheet--footerless'), 'cancel footer must not double safe area');
const css = fs.readFileSync(path.join(root, 'dashboard/static/css/13-responsive.css'), 'utf8');
assert.match(css, /\.bottom-sheet\.rs-dialog-sheet--footerless \.bottom-sheet-body\s*\{\s*padding-bottom: calc\(var\(--space-12\) \+ var\(--sab\)\);/);
console.log('progress sheet safe-area states passed');
