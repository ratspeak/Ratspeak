// Message Markdown: a CommonMark subset for LXMF FIELD_RENDERER = Markdown
// messages. Every byte of output HTML is built from escaped text.
(function() {
'use strict';

var MAX_SOURCE = 20000;
var MAX_BLOCK_DEPTH = 4;
var MAX_INLINE_DEPTH = 5;
var LINK_SCAN = 1000;
var RENDERER_MARKDOWN = 2;
var ZWSP = '​';
var ESCAPABLE = /[!-\/:-@\[-`{-~]/;
var PUNCT = /[!-\/:-@\[-`{-~\p{P}\p{S}]/u;
var SPACE = /\s/;
var URL_TAIL = /[*_~|]+$/;
var SPOILER_ATTRS = 'role="button" tabindex="0" aria-expanded="false" aria-label="Hidden spoiler"';
var COPY_ICON = '<svg viewBox="0 0 24 24" aria-hidden="true"><rect x="9" y="9" width="11" height="11" rx="2"/>' +
    '<path d="M5 15H4a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1h10a1 1 0 0 1 1 1v1"/></svg>';

var FENCE = /^( {0,3})(`{3,}|~{3,})(.*)$/;
var FENCE_CLOSE = /^ {0,3}(`{3,}|~{3,})[ \t]*$/;
var HEADING = /^( {0,3}(#{1,6})[ \t]+)(.*?)(?:[ \t]+#+)?[ \t]*$/;
var QUOTE = /^ {0,3}>(?: |$)/;
var SPOILER_LINE = /^ {0,3}>!/;
var LIST_ITEM = /^( *)(?:([-*+])|(\d{1,9})([.)]))([ \t]+)(?=\S)/;
var BLANK = /^[ \t]*$/;

function esc(s) {
    return String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
        .replace(/"/g, '&quot;').replace(/'/g, '&#39;');
}

function repeat(ch, n) {
    var out = '';
    while (n-- > 0) out += ch;
    return out;
}

// ---- Blocks -------------------------------------------------------------
// Lines carry their absolute source offset so wire rewrites can find them.

function toLines(text) {
    var parts = text.split('\n');
    var lines = [];
    var o = 0;
    for (var i = 0; i < parts.length; i++) {
        lines.push({ s: parts[i], o: o });
        o += parts[i].length + 1;
    }
    return lines;
}

function cut(line, n) {
    return { s: line.s.slice(n), o: line.o + n };
}

function blockSpoilerLine(s) {
    if (!SPOILER_LINE.test(s)) return false;
    return s.indexOf('!<', s.indexOf('>!') + 2) < 0;
}

function quoteLine(s) {
    return QUOTE.test(s) && !SPOILER_LINE.test(s);
}

function listMarker(s) {
    var m = LIST_ITEM.exec(s);
    if (!m) return null;
    return {
        indent: m[1].length,
        ordered: !m[2],
        bullet: m[2] || m[4],
        start: m[3] ? parseInt(m[3], 10) : 1,
        width: m[0].length,
    };
}

function fenceOpen(s) {
    var m = FENCE.exec(s);
    if (!m) return null;
    if (m[2].charAt(0) === '`' && m[3].indexOf('`') >= 0) return null;
    var lang = (m[3].trim().split(/\s+/)[0] || '').replace(/[^\w+#.-]/g, '').slice(0, 24);
    return { indent: m[1].length, fence: m[2], lang: lang };
}

function interrupts(s, inParagraph) {
    if (fenceOpen(s) || HEADING.test(s) || quoteLine(s) || blockSpoilerLine(s)) return true;
    var mk = listMarker(s);
    return !!mk && (!inParagraph || !mk.ordered || mk.start === 1);
}

function parseBlocks(lines, depth) {
    var blocks = [];
    var para = null;
    var i = 0;
    function flush() {
        if (para) blocks.push({ type: 'p', lines: para });
        para = null;
    }
    while (i < lines.length) {
        var line = lines[i];
        var s = line.s;
        var m;
        if (BLANK.test(s)) { flush(); i++; continue; }

        var fence = fenceOpen(s);
        if (fence) {
            flush();
            var body = [];
            var j = i + 1;
            var closed = false;
            for (; j < lines.length; j++) {
                var close = FENCE_CLOSE.exec(lines[j].s);
                if (close && close[1].charAt(0) === fence.fence.charAt(0) &&
                    close[1].length >= fence.fence.length) { closed = true; break; }
                var lead = /^ */.exec(lines[j].s)[0].length;
                body.push(lines[j].s.slice(Math.min(lead, fence.indent)));
            }
            blocks.push({ type: 'code', lang: fence.lang, text: body.join('\n') });
            i = closed ? j + 1 : j;
            continue;
        }

        if ((m = HEADING.exec(s)) && m[3]) {
            flush();
            blocks.push({ type: 'h', level: Math.min(m[2].length, 3), lines: [cut(line, m[1].length)] });
            i++;
            continue;
        }

        if (depth < MAX_BLOCK_DEPTH && (quoteLine(s) || blockSpoilerLine(s))) {
            flush();
            var spoiler = blockSpoilerLine(s);
            var from = line.o;
            var inner = [];
            while (i < lines.length && (spoiler ? blockSpoilerLine(lines[i].s) : quoteLine(lines[i].s))) {
                var marker = (spoiler ? /^ {0,3}>! ?/ : /^ {0,3}> ?/).exec(lines[i].s)[0];
                inner.push(cut(lines[i], marker.length));
                i++;
            }
            var last = lines[i - 1];
            blocks.push({
                type: spoiler ? 'spoiler' : 'quote',
                children: parseBlocks(inner, depth + 1),
                from: from,
                to: last.o + last.s.length,
            });
            continue;
        }

        var mk = listMarker(s);
        if (mk && depth < MAX_BLOCK_DEPTH && (!para || !mk.ordered || mk.start === 1)) {
            flush();
            var list = parseList(lines, i, depth);
            blocks.push(list.node);
            i = list.next;
            continue;
        }

        if (para && interrupts(s, true)) { flush(); continue; }
        var indent = /^ */.exec(s)[0].length;
        (para = para || []).push(cut(line, indent));
        i++;
    }
    flush();
    return blocks;
}

function parseList(lines, i, depth) {
    var first = listMarker(lines[i].s);
    var node = { type: first.ordered ? 'ol' : 'ul', start: first.start, items: [] };
    var base = first.indent;
    while (i < lines.length) {
        var mk = listMarker(lines[i].s);
        if (!mk || mk.indent < base || mk.indent >= base + 2 ||
            mk.ordered !== first.ordered || mk.bullet !== first.bullet) break;
        var item = { lines: [cut(lines[i], mk.width)], children: [] };
        var nested = [];
        var ended = false;
        i++;
        while (i < lines.length) {
            var s = lines[i].s;
            if (BLANK.test(s)) {
                var k = i + 1;
                while (k < lines.length && BLANK.test(lines[k].s)) k++;
                var next = k < lines.length ? listMarker(lines[k].s) : null;
                if (next && next.indent >= base) { i = k; continue; }
                ended = true;
                break;
            }
            var sub = listMarker(s);
            if (sub && sub.indent < base + 2) break;
            if (sub || nested.length) { nested.push(lines[i]); i++; continue; }
            if (interrupts(s, true)) { ended = true; break; }
            item.lines.push(cut(lines[i], /^ */.exec(s)[0].length));
            i++;
        }
        if (nested.length) item.children = parseBlocks(nested, depth + 1);
        node.items.push(item);
        if (ended) break;
    }
    return { node: node, next: i };
}

// ---- Inline -------------------------------------------------------------

function joinLines(lines) {
    var text = '';
    var map = [];
    for (var i = 0; i < lines.length; i++) {
        if (i) text += '\n';
        map.push({ at: text.length, o: lines[i].o });
        text += lines[i].s;
    }
    return { text: text, map: map };
}

function absolute(map, at) {
    var seg = map[0];
    for (var i = 1; i < map.length && map[i].at <= at; i++) seg = map[i];
    return seg ? seg.o + (at - seg.at) : at;
}

function charBefore(text, p) {
    var c = text.charAt(p - 1);
    return /[\uDC00-\uDFFF]/.test(c) && p > 1 ? text.slice(p - 2, p) : c;
}

function charAfter(text, p) {
    var c = text.charAt(p);
    return /[\uD800-\uDBFF]/.test(c) ? text.slice(p, p + 2) : c;
}

function charClass(ch) {
    if (!ch || SPACE.test(ch)) return 'space';
    return PUNCT.test(ch) ? 'punct' : 'word';
}

function linkAt(text, p) {
    var depth = 0;
    var j = p + 1;
    var limit = Math.min(text.length, p + 1 + LINK_SCAN);
    for (; j < limit; j++) {
        var c = text.charAt(j);
        if (c === '\\') { j++; continue; }
        if (c === '[') depth++;
        else if (c === ']') { if (!depth) break; depth--; }
    }
    if (j >= limit || text.charAt(j) !== ']' || text.charAt(j + 1) !== '(' || j === p + 1) return null;
    var k = j + 2;
    while (text.charAt(k) === ' ') k++;
    var dest;
    if (text.charAt(k) === '<') {
        var gt = text.indexOf('>', k);
        if (gt < 0) return null;
        dest = text.slice(k + 1, gt);
        k = gt + 1;
    } else {
        var parens = 0;
        var startDest = k;
        for (; k < text.length; k++) {
            var d = text.charAt(k);
            if (SPACE.test(d)) break;
            if (d === '\\') { k++; continue; }
            if (d === '(') parens++;
            else if (d === ')') { if (!parens) break; parens--; }
        }
        dest = text.slice(startDest, k);
    }
    while (text.charAt(k) === ' ') k++;
    if (text.charAt(k) !== ')') return null;
    dest = dest.replace(/\\([!-\/:-@\[-`{-~])/g, '$1');
    if (!/^https?:\/\/[^\s<>"]+$/i.test(dest)) return null;
    return { from: p + 1, to: j, href: dest, end: k + 1 };
}

function parseInline(text, ctx, depth, allowLinks, base) {
    base = base || 0;
    var nodes = [];
    var delims = [];
    var buf = '';
    var line = 0;
    var failedTicks = {};
    var nextShut = -2;
    var nextEol = -2;
    var ranges = allowLinks && ctx.links ? ctx.links(text) : [];
    var ri = 0;
    var p = 0;
    var n = text.length;

    function flush() {
        if (buf) nodes.push({ t: 'text', v: buf });
        buf = '';
    }
    function nested(from, to, links) {
        return depth < MAX_INLINE_DEPTH
            ? parseInline(text.slice(from, to), ctx, depth + 1, links, base + from)
            : [{ t: 'text', v: text.slice(from, to) }];
    }

    while (p < n) {
        while (ri < ranges.length && ranges[ri].end <= p) ri++;
        var range = ranges[ri];
        if (range && range.start === p) {
            // Prerendered ranges (paper links) are atomic: base64url may end in `_`.
            var label = range.html ? text.slice(range.start, range.end) : text.slice(range.start, range.end).replace(URL_TAIL, '');
            if (label) {
                var trimmed = range.end - range.start - label.length;
                flush();
                nodes.push({ t: 'auto', href: range.href.slice(0, range.href.length - trimmed), label: label, html: range.html || null });
                p += label.length;
                ri++;
                continue;
            }
        }
        var ch = text.charAt(p);
        if (ch === '\\' && p + 1 < n && ESCAPABLE.test(text.charAt(p + 1))) {
            buf += text.charAt(p + 1);
            p += 2;
            continue;
        }
        if (ch === '\n') {
            flush();
            nodes.push({ t: 'br' });
            line++;
            p++;
            continue;
        }
        if (ch === '`') {
            var run = /^`+/.exec(text.slice(p, p + 64))[0].length;
            var close = -1;
            if (!failedTicks[run]) {
                var q = p + run;
                while (q < n) {
                    var tick = text.indexOf('`', q);
                    if (tick < 0) break;
                    var len = 1;
                    while (text.charAt(tick + len) === '`') len++;
                    if (len === run) { close = tick; break; }
                    q = tick + len;
                }
                if (close < 0) failedTicks[run] = true;
            }
            if (close < 0) { buf += repeat('`', run); p += run; continue; }
            var code = text.slice(p + run, close).replace(/\n/g, ' ');
            if (/^ .*[^ ].* $/.test(code)) code = code.slice(1, -1);
            flush();
            nodes.push({ t: 'code', v: code });
            ctx.formatted = true;
            p = close + run;
            continue;
        }
        if (ch === '[' && allowLinks) {
            var link = linkAt(text, p);
            if (link) {
                flush();
                nodes.push({ t: 'link', href: link.href, kids: nested(link.from, link.to, false) });
                ctx.formatted = true;
                p = link.end;
                continue;
            }
        }
        if (ch === '>' && text.charAt(p + 1) === '!') {
            if (nextShut !== -1 && nextShut < p + 2) nextShut = text.indexOf('!<', p + 2);
            if (nextEol !== -1 && nextEol < p) nextEol = text.indexOf('\n', p);
            var eol = nextEol;
            var shut = nextShut;
            if (shut >= 0 && (eol < 0 || shut < eol) && text.slice(p + 2, shut).trim()) {
                var from = p + 2;
                var to = shut;
                while (SPACE.test(text.charAt(from))) from++;
                while (SPACE.test(text.charAt(to - 1))) to--;
                flush();
                if (ctx.spans) ctx.spans.push({ start: base + p, end: base + shut + 2 });
                nodes.push({ t: 'spoiler', kids: nested(from, to, allowLinks) });
                ctx.formatted = true;
                p = shut + 2;
                continue;
            }
        }
        if (ch === '*' || ch === '_' || ch === '~' || ch === '|') {
            var count = 1;
            while (text.charAt(p + count) === ch) count++;
            var before = charClass(charBefore(text, p));
            var after = charClass(charAfter(text, p + count));
            var left = after !== 'space' && (after !== 'punct' || before !== 'word');
            var right = before !== 'space' && (before !== 'punct' || after !== 'word');
            var open = left;
            var shut2 = right;
            if (ch === '_') {
                open = left && (!right || before === 'punct');
                shut2 = right && (!left || after === 'punct');
            }
            flush();
            var delim = { t: 'delim', ch: ch, n: count, orig: count, open: open, close: shut2, at: base + p, line: line };
            delim.di = delims.length;
            delims.push(delim);
            nodes.push(delim);
            p += count;
            continue;
        }
        buf += ch;
        p++;
    }
    flush();
    return resolveDelims(nodes, delims, ctx);
}

function resolveDelims(nodes, delims, ctx) {
    for (var i = 0; i < nodes.length; i++) {
        nodes[i].prev = nodes[i - 1] || null;
        nodes[i].next = nodes[i + 1] || null;
    }
    var head = { next: nodes[0] || null };
    if (nodes[0]) nodes[0].prev = head;
    var bottoms = {};
    for (var ci = 0; ci < delims.length; ci++) {
        var c = delims[ci];
        while (!c.dead && c.close && c.n > 0) {
            var emphasis = c.ch === '*' || c.ch === '_';
            var key = c.ch + (c.open ? 'o' : '') + (c.orig % 3);
            var bottom = bottoms[key] == null ? -1 : bottoms[key];
            var o = null;
            for (var j = ci - 1; j > bottom; j--) {
                var cand = delims[j];
                if (cand.dead || !cand.open || cand.ch !== c.ch || cand.n === 0) continue;
                if (emphasis) {
                    if ((c.open || cand.close) && (cand.orig + c.orig) % 3 === 0 &&
                        !(cand.orig % 3 === 0 && c.orig % 3 === 0)) continue;
                } else if (cand.orig !== 2 || c.orig !== 2 || cand.n !== 2 || c.n !== 2 ||
                    (c.ch === '|' && cand.line !== c.line)) {
                    continue;
                }
                o = cand;
                break;
            }
            if (!o) {
                bottoms[key] = ci - 1;
                if (!c.open) c.dead = true;
                break;
            }
            var use = emphasis ? (o.n >= 2 && c.n >= 2 ? 2 : 1) : 2;
            var type = c.ch === '~' ? 'del' : c.ch === '|' ? 'spoiler' : use === 2 ? 'strong' : 'em';
            o.n -= use;
            c.n -= use;
            if (type === 'spoiler' && ctx.spoilers) ctx.spoilers.push({ open: o.at, close: c.at });
            if (type === 'spoiler' && ctx.spans) ctx.spans.push({ start: o.at, end: c.at + 2 });
            ctx.formatted = true;
            var kids = [];
            for (var walk = o.next; walk && walk !== c; walk = walk.next) kids.push(walk);
            var el = { t: type, kids: kids, prev: o, next: c };
            o.next = el;
            c.prev = el;
            for (var k = o.di + 1; k < ci; k++) delims[k].dead = true;
            if (o.n === 0) unlink(o);
            if (c.n === 0) { unlink(c); c.dead = true; }
        }
    }
    var out = [];
    for (var node = head.next; node; node = node.next) out.push(node);
    return out;
}

function unlink(node) {
    if (node.prev) node.prev.next = node.next;
    if (node.next) node.next.prev = node.prev;
}

// ---- Output -------------------------------------------------------------

function inlineHtml(nodes) {
    var out = '';
    for (var i = 0; i < nodes.length; i++) {
        var nd = nodes[i];
        switch (nd.t) {
        case 'text': out += esc(nd.v); break;
        case 'br': out += '<br>'; break;
        case 'delim': out += esc(repeat(nd.ch, nd.n)); break;
        case 'code': out += '<code class="md-code">' + esc(nd.v) + '</code>'; break;
        case 'em': out += '<em>' + inlineHtml(nd.kids) + '</em>'; break;
        case 'strong': out += '<strong>' + inlineHtml(nd.kids) + '</strong>'; break;
        case 'del': out += '<del>' + inlineHtml(nd.kids) + '</del>'; break;
        case 'spoiler':
            out += '<span class="md-spoiler" ' + SPOILER_ATTRS + '>' +
                '<span class="md-spoiler-content" aria-hidden="true" inert>' + inlineHtml(nd.kids) + '</span></span>';
            break;
        case 'auto':
            if (nd.html) { out += nd.html; break; }
            out += '<a class="rs-message-link" href="' + esc(nd.href) + '" data-url="' + esc(nd.href) +
                '" rel="noopener noreferrer">' + esc(nd.label) + '</a>';
            break;
        case 'link':
            out += '<a class="rs-message-link md-link" href="' + esc(nd.href) + '" data-url="' + esc(nd.href) +
                '" data-masked="1" rel="noopener noreferrer">' + inlineHtml(nd.kids) + '</a>';
            break;
        }
    }
    return out;
}

function inlineText(nodes, spoiler) {
    var out = '';
    for (var i = 0; i < nodes.length; i++) {
        var nd = nodes[i];
        if (nd.t === 'text') out += nd.v;
        else if (nd.t === 'br') out += '\n';
        else if (nd.t === 'delim') out += repeat(nd.ch, nd.n);
        else if (nd.t === 'code') out += nd.v;
        else if (nd.t === 'auto') out += nd.label;
        else if (nd.t === 'spoiler') out += spoiler;
        else out += inlineText(nd.kids, spoiler);
    }
    return out;
}

function inlineOf(lines, ctx) {
    var joined = joinLines(lines);
    var spoilers = ctx.spoilers ? [] : null;
    var spans = ctx.spans ? [] : null;
    var sub = { links: ctx.links, spoilers: spoilers, spans: spans, formatted: false };
    var nodes = parseInline(joined.text, sub, 0, true);
    if (sub.formatted) ctx.formatted = true;
    if (spans) {
        spans.forEach(function(sp) {
            ctx.spans.push({ start: absolute(joined.map, sp.start), end: absolute(joined.map, sp.end - 1) + 1 });
        });
    }
    if (spoilers && spoilers.length) {
        spoilers.forEach(function(sp) {
            var atStart = sp.open === 0 || joined.text.charAt(sp.open - 1) === '\n';
            ctx.spoilers.push({
                open: absolute(joined.map, sp.open),
                close: absolute(joined.map, sp.close),
                lineStart: atStart,
            });
        });
    }
    return nodes;
}

function blocksHtml(blocks, ctx) {
    var out = '';
    blocks.forEach(function(b) {
        if (b.type === 'p') {
            out += '<p class="md-p">' + inlineHtml(inlineOf(b.lines, ctx)) + '</p>';
        } else if (b.type === 'h') {
            out += '<h' + b.level + ' class="md-h md-h' + b.level + '">' + inlineHtml(inlineOf(b.lines, ctx)) + '</h' + b.level + '>';
        } else if (b.type === 'code') {
            out += '<div class="md-codeblock"><div class="md-code-head">' +
                (b.lang ? '<span class="md-code-lang">' + esc(b.lang) + '</span>' : '') +
                '<button type="button" class="md-code-copy" aria-label="Copy code">' + COPY_ICON + '</button></div>' +
                '<pre class="md-pre"' + (b.lang ? ' data-lang="' + esc(b.lang) + '"' : '') + '><code>' + esc(b.text) + '</code></pre></div>';
        } else if (b.type === 'quote') {
            out += '<blockquote class="md-quote">' + blocksHtml(b.children, ctx) + '</blockquote>';
        } else if (b.type === 'spoiler') {
            if (ctx.spans) ctx.spans.push({ start: b.from, end: b.to });
            out += '<div class="md-spoiler md-spoiler-block" ' + SPOILER_ATTRS + '>' +
                '<div class="md-spoiler-content" aria-hidden="true" inert>' + blocksHtml(b.children, ctx) + '</div></div>';
        } else {
            var tag = b.type;
            out += '<' + tag + ' class="md-list"' + (tag === 'ol' && b.start !== 1 ? ' start="' + b.start + '"' : '') + '>';
            b.items.forEach(function(item) {
                out += '<li>' + inlineHtml(inlineOf(item.lines, ctx)) + blocksHtml(item.children, ctx) + '</li>';
            });
            out += '</' + tag + '>';
        }
    });
    return out;
}

function blocksText(blocks, ctx, spoiler) {
    var parts = [];
    blocks.forEach(function(b) {
        if (b.type === 'p' || b.type === 'h') parts.push(inlineText(inlineOf(b.lines, ctx), spoiler));
        else if (b.type === 'code') parts.push(b.text);
        else if (b.type === 'quote') parts.push(blocksText(b.children, ctx, spoiler));
        else if (b.type === 'spoiler') parts.push(spoiler);
        else {
            b.items.forEach(function(item, idx) {
                var marker = b.type === 'ol' ? (b.start + idx) + '. ' : '• ';
                var body = inlineText(inlineOf(item.lines, ctx), spoiler);
                var sub = blocksText(item.children, ctx, spoiler);
                parts.push(marker + body + (sub ? '\n' + sub : ''));
            });
        }
    });
    return parts.join('\n');
}

function blockFormatted(blocks) {
    return blocks.some(function(b) { return b.type !== 'p'; });
}

function parse(text) {
    return parseBlocks(toLines(String(text || '').replace(/\r\n?/g, '\n')), 0);
}

// Wire form: composer `||x||` becomes Sideband/mistune `>!x!<`. A zero-width
// space keeps a line-leading spoiler from parsing as a mistune block spoiler.
function toWire(text, spoilers) {
    if (!spoilers.length) return text;
    var edits = [];
    spoilers.forEach(function(sp) {
        edits.push({ at: sp.open, put: (sp.lineStart ? ZWSP : '') + '>!' });
        edits.push({ at: sp.close, put: '!<' });
    });
    edits.sort(function(a, b) { return b.at - a.at; });
    var out = text;
    edits.forEach(function(e) { out = out.slice(0, e.at) + e.put + out.slice(e.at + 2); });
    return out;
}

function replaceRange(field, from, to, text) {
    field.focus();
    field.setSelectionRange(from, to);
    var done = false;
    try { done = document.execCommand('insertText', false, text); } catch (_) {}
    if (!done) {
        field.setRangeText(text, from, to, 'end');
        field.dispatchEvent(new Event('input', { bubbles: true }));
    }
}

function surroundedBy(value, from, to, marker) {
    var m = marker.length;
    if (value.slice(from - m, from) !== marker || value.slice(to, to + m) !== marker) return false;
    if (marker !== '*') return true;
    // `*` must not be half of `**`: count the run on each side (1 or 3 is italic).
    var left = 0;
    while (value.charAt(from - 1 - left) === '*') left++;
    var right = 0;
    while (value.charAt(to + right) === '*') right++;
    return left === right && left % 2 === 1;
}

// Composer shortcut: wrap the selection in a marker, or unwrap it if it is
// already wrapped. One insertText per press keeps native undo to one step.
function toggleWrap(field, marker) {
    var value = field.value;
    var start = field.selectionStart;
    var end = field.selectionEnd;
    var m = marker.length;
    var selected = value.slice(start, end);

    if (start === end) {
        if (surroundedBy(value, start, end, marker)) {
            replaceRange(field, start - m, end + m, '');
            field.setSelectionRange(start - m, start - m);
            return;
        }
        replaceRange(field, start, end, marker + marker);
        field.setSelectionRange(start + m, start + m);
        return;
    }
    if (surroundedBy(value, start, end, marker)) {
        replaceRange(field, start - m, end + m, selected);
        field.setSelectionRange(start - m, end - m);
        return;
    }
    if (selected.length > 2 * m && selected.slice(0, m) === marker && selected.slice(-m) === marker) {
        var inner = selected.slice(m, -m);
        replaceRange(field, start, end, inner);
        field.setSelectionRange(start, start + inner.length);
        return;
    }
    var lead = /^\s*/.exec(selected)[0].length;
    var trail = /\s*$/.exec(selected)[0].length;
    var core = selected.slice(lead, selected.length - trail);
    if (!core) return;
    var from = start + lead;
    var to = end - trail;
    var text;
    if (core.indexOf('\n') >= 0) {
        text = marker === '`'
            ? '```\n' + core + '\n```'
            : core.split('\n').map(function(line) {
                var l = /^\s*/.exec(line)[0];
                var r = /\s*$/.exec(line)[0];
                var body = line.slice(l.length, line.length - r.length);
                return body ? l + marker + body + marker + r : line;
            }).join('\n');
        if (marker === '`') {
            var before = from > 0 && value.charAt(from - 1) !== '\n' ? '\n' : '';
            var after = to < value.length && value.charAt(to) !== '\n' ? '\n' : '';
            text = before + text + after;
        }
        replaceRange(field, from, to, text);
        field.setSelectionRange(from, from + text.length);
        return;
    }
    replaceRange(field, from, to, marker + core + marker);
    field.setSelectionRange(from + m, to + m);
}

window.RS = window.RS || {};
window.RS.markdown = {
    RENDERER: RENDERER_MARKDOWN,
    toggleWrap: toggleWrap,
    MAX_SOURCE: MAX_SOURCE,
    render: function(text, opts) {
        if (String(text || '').length > MAX_SOURCE) return null;
        var ctx = { links: opts && opts.links };
        return blocksHtml(parse(text), ctx);
    },
    strip: function(text, opts) {
        var placeholder = opts && opts.spoiler != null ? opts.spoiler : '[Spoiler]';
        var out = blocksText(parse(text), { links: null }, placeholder);
        return out.charAt(0) === ZWSP ? out.slice(1) : out;
    },
    // Copy keeps the source but replaces spoilers the reader has not revealed.
    // `revealed(i)` takes the spoiler's document-order index (as rendered).
    redactSpoilers: function(text, revealed) {
        var source = String(text || '').replace(/\r\n?/g, '\n');
        if (source.length > MAX_SOURCE) return source;
        var ctx = { links: null, spans: [] };
        blocksHtml(parse(source), ctx);
        var spans = ctx.spans.sort(function(a, b) { return a.start - b.start || b.end - a.end; });
        var cut = [];
        var coveredUntil = -1;
        spans.forEach(function(sp, i) {
            if (sp.start < coveredUntil || (revealed && revealed(i))) return;
            cut.push(sp);
            coveredUntil = sp.end;
        });
        for (var k = cut.length - 1; k >= 0; k--) {
            var start = cut[k].start;
            if (source.charAt(start - 1) === ZWSP) start--;
            source = source.slice(0, start) + '[Spoiler]' + source.slice(cut[k].end);
        }
        return source;
    },
    // Formatting decides FIELD_RENDERER on send; escapes alone never do, so
    // a bare ¯\_(ツ)_/¯ still goes out as plain text.
    prepare: function(text, opts) {
        var source = String(text || '').replace(/\r\n?/g, '\n');
        if (source.length > MAX_SOURCE) return { markdown: false, content: source };
        var blocks = parse(source);
        var ctx = { links: opts && opts.links, spoilers: [], formatted: blockFormatted(blocks) };
        blocksHtml(blocks, ctx);
        return ctx.formatted
            ? { markdown: true, content: toWire(source, ctx.spoilers) }
            : { markdown: false, content: source };
    },
};
})();
