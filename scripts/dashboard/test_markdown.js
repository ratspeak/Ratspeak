#!/usr/bin/env node
// Message Markdown: escaped-only output, the CommonMark subset, spoilers on the
// Sideband wire form, FIELD_RENDERER decisions, previews and pathological input.
'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const root = path.resolve(__dirname, '..', '..');
const lxmf = fs.readFileSync(path.join(root, 'dashboard/static/js/lxmf.js'), 'utf8');
const context = { window: {} };
vm.createContext(context);
vm.runInContext(fs.readFileSync(path.join(root, 'dashboard/static/js/markdown.js'), 'utf8'), context);
const linkStart = lxmf.indexOf('var RS_LINK_TLDS');
const linkEnd = lxmf.indexOf('function linkifyMessageText');
assert(linkStart > 0 && linkEnd > linkStart, 'link ranges must stay extractable');
vm.runInContext(lxmf.slice(linkStart, linkEnd), context);
const md = context.window.RS.markdown;
const links = context._messageLinkRanges;
const render = (text) => md.render(text, { links });
const prepare = (text) => JSON.parse(JSON.stringify(md.prepare(text, { links })));

// Inline
assert.equal(render('**b** *i* _i_ ***bi*** ~~s~~ `c`'),
    '<p class="md-p"><strong>b</strong> <em>i</em> <em>i</em> <em><strong>bi</strong></em> <del>s</del> <code class="md-code">c</code></p>');
assert.equal(render('__b__'), '<p class="md-p"><strong>b</strong></p>', '__ is CommonMark bold, not underline');
assert.equal(render('snake_case_name'), '<p class="md-p">snake_case_name</p>');
assert.equal(render('**unclosed'), '<p class="md-p">**unclosed</p>');
assert.equal(render('a * b'), '<p class="md-p">a * b</p>');
assert.equal(render('\\*not\\*'), '<p class="md-p">*not*</p>');
assert.equal(render('line1\nline2'), '<p class="md-p">line1<br>line2</p>');
assert.equal(render('`` a ` b ``'), '<p class="md-p"><code class="md-code">a ` b</code></p>');

// Links: http(s) only, URLs stay atomic, masked links are marked.
assert.equal(render('[site](https://example.com/a_(b))'),
    '<p class="md-p"><a class="rs-message-link md-link" href="https://example.com/a_(b)" data-url="https://example.com/a_(b)" data-masked="1" rel="noopener noreferrer">site</a></p>');
assert.equal(render('https://x.com/a*b*c'),
    '<p class="md-p"><a class="rs-message-link" href="https://x.com/a*b*c" data-url="https://x.com/a*b*c" rel="noopener noreferrer">https://x.com/a*b*c</a></p>');
assert.equal(render('**example.com**'),
    '<p class="md-p"><strong><a class="rs-message-link" href="https://example.com" data-url="https://example.com" rel="noopener noreferrer">example.com</a></strong></p>');
assert.equal(render('[x](javascript:alert(1))'), '<p class="md-p">[x](javascript:alert(1))</p>');
assert.equal(render('[x](data:text/html,hi)'), '<p class="md-p">[x](data:text/html,hi)</p>');
assert.equal(render('[](https://a.com)'), '<p class="md-p">[](<a class="rs-message-link" href="https://a.com" data-url="https://a.com" rel="noopener noreferrer">https://a.com</a>)</p>');
assert.match(render('[**bold** link](https://a.com)'), /<a [^>]*data-masked="1"[^>]*><strong>bold<\/strong> link<\/a>/);

// Blocks
assert.equal(render('# H1\n## H2\n#### H4'),
    '<h1 class="md-h md-h1">H1</h1><h2 class="md-h md-h2">H2</h2><h3 class="md-h md-h3">H4</h3>');
assert.equal(render('#hashtag'), '<p class="md-p">#hashtag</p>');
assert.equal(render('- a\n- b\n  - c'),
    '<ul class="md-list"><li>a</li><li>b<ul class="md-list"><li>c</li></ul></li></ul>');
assert.equal(render('3. x\n4. y'), '<ol class="md-list" start="3"><li>x</li><li>y</li></ol>');
assert.equal(render('Total\n2. no list'), '<p class="md-p">Total<br>2. no list</p>', 'only 1. interrupts a paragraph');
assert.equal(render('> q\n> r\n\nafter'),
    '<blockquote class="md-quote"><p class="md-p">q<br>r</p></blockquote><p class="md-p">after</p>');
assert.equal(render('>:('), '<p class="md-p">&gt;:(</p>', 'a quote needs a space after >');
const codeBlock = (lang, code) => {
    const html = render('```' + lang + '\n' + code + '\n```');
    assert(html.startsWith('<div class="md-codeblock"><div class="md-code-head">'), html);
    assert(html.includes('<button type="button" class="md-code-copy" aria-label="Copy code">'), html);
    return html.slice(html.indexOf('<pre'), html.lastIndexOf('</div>'));
};
assert.equal(codeBlock('js', 'if (a < b) {}'), '<pre class="md-pre" data-lang="js"><code>if (a &lt; b) {}</code></pre>');
assert(render('```js\nx\n```').includes('<span class="md-code-lang">js</span>'));
assert(!render('```\nx\n```').includes('md-code-lang'), 'no label without a language');
assert(render('```\n**raw**').endsWith('<pre class="md-pre"><code>**raw**</code></pre></div>'), 'unclosed fences run to the end');
assert.equal(codeBlock('"><img src=x>', 'x'), '<pre class="md-pre" data-lang="img"><code>x</code></pre>');
assert.equal(render('a\n---\nb'), '<p class="md-p">a<br>---<br>b</p>', 'no setext headings or rules');
assert.equal(render('Title\n==='), '<p class="md-p">Title<br>===</p>');

// Spoilers: composer form, inline wire form, block wire form.
const spoiler = (inner) => '<span class="md-spoiler" role="button" tabindex="0" aria-expanded="false" aria-label="Hidden spoiler">' +
    '<span class="md-spoiler-content" aria-hidden="true" inert>' + inner + '</span></span>';
assert.equal(render('a ||x|| b'), '<p class="md-p">a ' + spoiler('x') + ' b</p>');
assert.equal(render('a >!x!< b'), '<p class="md-p">a ' + spoiler('x') + ' b</p>');
assert.equal(render('>!x!< tail'), '<p class="md-p">' + spoiler('x') + ' tail</p>');
assert.equal(render('||a\nb||'), '<p class="md-p">||a<br>b||</p>', 'spoilers stay on one line, like mistune');
assert.match(render('>! hidden\n>! more'), /^<div class="md-spoiler md-spoiler-block"[^>]*><div class="md-spoiler-content" aria-hidden="true" inert><p class="md-p">hidden<br>more<\/p><\/div><\/div>$/);

// Nothing executable survives.
const hostile = [
    '<script>alert(1)</script>', '<img src=x onerror=alert(1)>', '[x](https://a.com "onmouseover=alert(1)")',
    '[x](https://a.com/"onmouseover="alert(1))', '`<b>`', '# <i>h</i>', '- <u>l</u>', '> <svg onload=alert(1)>',
    '||<a href=javascript:x>s</a>||', '[<img>](https://a.com/<x>)', 'https://a.com/"><script>',
];
const TAGS = /^(p|br|strong|em|del|code|pre|h[1-3]|ul|ol|li|blockquote|span|div|a|button|svg|rect|path)$/;
const ATTRS = /^(class|href|data-url|data-masked|data-lang|rel|role|tabindex|start|type|viewBox|x|y|width|height|rx|d|inert|aria-(expanded|label|hidden))$/;
hostile.forEach((text) => {
    const html = render(text);
    const tags = html.match(/<[^>]*>/g) || [];
    tags.forEach((tag) => {
        const m = /^<\/?([a-z0-9]+)((?:\s+[a-zA-Z-]+(?:="[^"<>]*")?)*)\s*\/?>$/.exec(tag);
        assert(m && TAGS.test(m[1]), text + ' → tag ' + tag);
        (m[2].replace(/="[^"]*"/g, '').match(/[a-zA-Z-]+/g) || []).forEach((attr) => assert(ATTRS.test(attr), text + ' → attr ' + attr));
        const href = /href="([^"]*)"/.exec(tag);
        if (href) assert(/^https?:\/\//.test(href[1]), text + ' → href ' + href[1]);
    });
});

// Strip for previews; spoilers never leak.
assert.equal(md.strip('# Hi\n**bold** and ||secret||'), 'Hi\nbold and [Spoiler]');
assert.equal(md.strip('- a\n- b\n1. c'), '• a\n• b\n1. c');
assert.equal(md.strip('​>!x!< after'), '[Spoiler] after');
assert.equal(md.strip('>! block\n\nshown'), '[Spoiler]\nshown');
assert.equal(md.strip('[text](https://a.com)'), 'text');

// FIELD_RENDERER decision and wire form.
assert.deepEqual(prepare('hello there'), { markdown: false, content: 'hello there' });
assert.deepEqual(prepare('¯\\_(ツ)_/¯'), { markdown: false, content: '¯\\_(ツ)_/¯' }, 'escapes alone stay plain');
assert.deepEqual(prepare('see https://x.com/a_b_c'), { markdown: false, content: 'see https://x.com/a_b_c' });
assert.deepEqual(prepare('**unclosed'), { markdown: false, content: '**unclosed' });
assert.deepEqual(prepare('a **b**'), { markdown: true, content: 'a **b**' });
assert.deepEqual(prepare('a ||x|| b'), { markdown: true, content: 'a >!x!< b' });
assert.deepEqual(prepare('||x|| b\n||y||'), { markdown: true, content: '​>!x!< b\n​>!y!<' });
assert.deepEqual(prepare('- ||x||\n> ||y||'), { markdown: true, content: '- ​>!x!<\n> ​>!y!<' });
assert.deepEqual(prepare('`||code||` ||s||'), { markdown: true, content: '`||code||` >!s!<' });
assert.deepEqual(prepare('[||s|| link](https://a.com)'), { markdown: true, content: '[>!s!< link](https://a.com)' });
assert.deepEqual(prepare('\\||x||'), { markdown: false, content: '\\||x||' });
const wire = prepare('||a|| and ||b||').content;
assert.equal(render(wire), '<p class="md-p">​' + spoiler('a') + ' and ' + spoiler('b') + '</p>');

// Pathological input stays fast; oversize input falls back to plain text.
const hostileSizes = [
    '*'.repeat(19000), '_a'.repeat(9000), '['.repeat(19000), '`'.repeat(19000), '`a'.repeat(9000),
    '>!'.repeat(9000), '- '.repeat(9000) + 'x', '> '.repeat(9000) + 'x', '**a'.repeat(6000), '|'.repeat(19000),
    '[a](https://a.com/' + '('.repeat(18000), 'a\n'.repeat(9000), '  - x\n'.repeat(3000),
];
hostileSizes.forEach((text) => {
    const t0 = Date.now();
    render(text);
    md.prepare(text, { links });
    assert(Date.now() - t0 < 1500, 'slow input: ' + JSON.stringify(text.slice(0, 20)) + ' took ' + (Date.now() - t0) + 'ms');
});
assert.equal(md.render('x'.repeat(md.MAX_SOURCE + 1), { links }), null);
assert.equal(md.prepare('**' + 'x'.repeat(md.MAX_SOURCE) + '**', { links }).markdown, false);

// Copy redaction: source is kept, covered spoilers become [Spoiler], and the
// index matches each spoiler's rendered (document) order.
const mixed = 'Guess: \u200B>!fire lookout!< and ||two|| ok\n>! block one\n>! block two\nafter\n- item >!x!<\n||a >!b!< c||';
assert.equal(md.redactSpoilers(mixed), 'Guess: [Spoiler] and [Spoiler] ok\n[Spoiler]\nafter\n- item [Spoiler]\n[Spoiler]');
assert.equal(md.redactSpoilers(mixed, () => true), mixed);
assert.equal(md.redactSpoilers(mixed, (i) => i === 1), 'Guess: [Spoiler] and ||two|| ok\n[Spoiler]\nafter\n- item [Spoiler]\n[Spoiler]');
assert.equal(md.redactSpoilers(mixed, (i) => i !== 5), 'Guess: \u200B>!fire lookout!< and ||two|| ok\n>! block one\n>! block two\nafter\n- item >!x!<\n||a [Spoiler] c||',
    'a revealed outer spoiler keeps a covered inner one hidden');
assert.equal(md.redactSpoilers('`>!code!<` >!real!<'), '`>!code!<` [Spoiler]', 'code is never a spoiler');
const order = render(mixed).match(/md-spoiler-content[^>]*>([^<]*)/g).map((t) => t.replace(/.*>/, ''));
assert.equal(order.length, 6, 'six spoilers render, matching six redaction indexes');
assert.deepEqual(order.slice(0, 2), ['fire lookout', 'two']);
assert(!md.redactSpoilers(mixed, (j) => j !== 0).includes('fire lookout'), 'index 0 is the first rendered spoiler');
assert(md.redactSpoilers(mixed, (j) => j !== 1).includes(' and [Spoiler] ok'), 'index 1 is the second rendered spoiler');

// Composer shortcuts: wrap, unwrap, empty pair, multi-line, `*` vs `**`.
context.document = { execCommand: () => false };
context.Event = class { constructor(type) { this.type = type; } };
function field(value, start, end) {
    return {
        value, selectionStart: start, selectionEnd: end === undefined ? start : end,
        focus() {}, dispatchEvent() {},
        setSelectionRange(a, b) { this.selectionStart = a; this.selectionEnd = b; },
        setRangeText(text, a, b) { this.value = this.value.slice(0, a) + text + this.value.slice(b); },
    };
}
function press(f, marker) { md.toggleWrap(f, marker); return [f.value, f.value.slice(f.selectionStart, f.selectionEnd), f.selectionStart]; }
let f = field('say hi now', 4, 6);
assert.deepEqual(press(f, '**'), ['say **hi** now', 'hi', 6]);
assert.deepEqual(press(f, '**'), ['say hi now', 'hi', 4], 'second press unwraps');
f = field('a **hi** b', 4, 6);
assert.deepEqual(press(f, '*'), ['a ***hi*** b', 'hi', 5], 'italic inside bold wraps, not unwraps');
f = field('say  hi  now', 3, 9);
assert.deepEqual(press(f, '*'), ['say  *hi*  now', 'hi', 6], 'surrounding whitespace stays outside');
f = field('ab', 1);
assert.deepEqual(press(f, '~~'), ['a~~~~b', '', 3]);
assert.deepEqual(press(f, '~~'), ['ab', '', 1], 'empty pair toggles off');
f = field('one\n\ntwo', 0, 8);
assert.deepEqual(press(f, '**')[0], '**one**\n\n**two**', 'lines wrap separately');
f = field('x\nfn a()\nfn b()\ny', 2, 16);
assert.equal(press(f, '`')[0], 'x\n```\nfn a()\nfn b()\n```\ny', 'multi-line code becomes a fence');
f = field('*hi*', 0, 4);
assert.deepEqual(press(f, '*'), ['hi', 'hi', 0], 'a selection that includes its markers unwraps');

// Paper links render as one atomic "Paper message" link; the full URI stays in data-url.
const paperUri = 'lxm://AbC_d-9__';
const paperHtml = render('**see** ' + paperUri + ' and https://x.com');
assert(paperHtml.includes('<a class="rs-message-link rs-paper-link" href="#" data-url="' + paperUri + '"'),
    'paper links keep the trailing underscore and full URI');
assert(paperHtml.includes('<span>Paper message</span></a>'));
assert(!paperHtml.includes('>' + paperUri + '<'), 'the base64 body is never shown as link text');
assert(paperHtml.includes('<strong>see</strong>') && paperHtml.includes('data-url="https://x.com"'));
assert.equal(md.strip('a ' + paperUri), 'a ' + paperUri, 'previews keep the source text');
assert.deepEqual(links('xlxm://abc').length, 0, 'lxm:// must start a word');
assert.deepEqual(prepare('_' + paperUri + '_').markdown, false, 'underscores inside a paper link are not emphasis');

console.log('markdown ok');
