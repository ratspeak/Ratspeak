#!/usr/bin/env node
// The shared QR encoder: the contact card stays byte-identical (fixed v13-Q,
// mask 0), every version 1-40 at every ECC level decodes with the vendored
// jsQR, and paper links pick their symbol by the size rule.
'use strict';

const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const js = path.join(__dirname, '..', '..', 'dashboard', 'static', 'js');
const jsQR = require(path.join(js, 'vendor', 'jsQR.js'));

function Path2D(d) { this.d = d; }
const context = { window: { RS: {}, TextEncoder, jsQR }, TextEncoder, Path2D, Math, Error };
vm.runInNewContext(fs.readFileSync(path.join(js, 'contact_card.js'), 'utf8'), context);
const qr = context.window.RS.qr;
const contactSource = fs.readFileSync(path.join(js, 'contact_card.js'), 'utf8');

// A canvas that records every 2D call, so drawing changes are visible.
function recordingCanvas() {
    const ops = [];
    const rects = [];
    const ctx = new Proxy({}, {
        get(target, key) {
            if (key in target) return target[key];
            return function(...args) {
                if (key === 'fillRect') rects.push(args);
                ops.push([key].concat(args.map(a => a instanceof Path2D ? 'P' + a.d.length : a)));
            };
        },
        set(target, key, value) { ops.push(['set', key, value]); return true; }
    });
    return { width: 0, height: 0, ops, rects, getContext: () => ctx };
}

// Golden digests of the pre-generalisation renderer (fixed v13-Q encoder).
const CONTACT_GOLDEN = [
    ['ratspeak://contact?v=1&a=0123456789abcdef0123456789abcdef', '797885dfeb64649fe3d6cc7b4de939ca18198f4aa402c735e06f7b89efaacfa1'],
    ['x'.repeat(150), 'ed9eb47039dab14170ed8f269e352446f5b5ba067544d4e0827e9587933960c1'],
    ['R'.repeat(241), '408686d33180f163ae1793ee3b5e9e2349087eb6251f8706f6be288a2873c9e3'],
    ['Ünïcødé rat 🐀 '.repeat(8), '662b8408b9337ea0b37f201f1d1c2a5ec2af65eba1d522d1b7ddc963a6ff4927'],
];
for (const [payload, digest] of CONTACT_GOLDEN) {
    const canvas = recordingCanvas();
    qr.renderCanvas(canvas, payload);
    assert.equal(canvas.width, 900);
    assert.equal(crypto.createHash('sha256').update(JSON.stringify(canvas.ops)).digest('hex'), digest,
        'contact card drawing must stay byte-identical for a ' + payload.length + '-char payload');
}
assert.throws(() => qr.renderCanvas(recordingCanvas(), 'R'.repeat(242)), /too large/,
    'the contact card keeps its fixed v13-Q ceiling');

function raster(symbol, ppm) {
    const quiet = 4;
    const n = (symbol.size + quiet * 2) * ppm;
    const data = new Uint8ClampedArray(n * n * 4).fill(255);
    for (let y = 0; y < symbol.size; y++) {
        for (let x = 0; x < symbol.size; x++) {
            if (!symbol.modules[y][x]) continue;
            for (let dy = 0; dy < ppm; dy++) {
                for (let dx = 0; dx < ppm; dx++) {
                    const i = (((y + quiet) * ppm + dy) * n + (x + quiet) * ppm + dx) * 4;
                    data[i] = data[i + 1] = data[i + 2] = 0;
                }
            }
        }
    }
    return { data, n };
}

function decode(symbol, ppm) {
    const image = raster(symbol, ppm || (symbol.size > 140 ? 2 : 3));
    return jsQR(image.data, image.n, image.n);
}

const ALPHABET = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_';
function paperUri(length, seed) {
    let s = 'lxm://';
    while (s.length < length) s += ALPHABET[(s.length * 7 + (seed || 0)) % 64];
    return s;
}

// Full capacity of every version and ECC level, best mask.
for (let version = 1; version <= 40; version++) {
    for (const ecc of ['L', 'M', 'Q', 'H']) {
        const text = paperUri(qr.byteCapacity(version, ecc), version);
        const symbol = qr.encode(text, { version, ecc });
        assert.equal(symbol.size, version * 4 + 17);
        const result = decode(symbol);
        assert(result && result.data === text && result.version === version,
            'v' + version + '-' + ecc + ' must decode at full capacity');
    }
}
assert.throws(() => qr.encode(paperUri(qr.byteCapacity(5, 'L') + 1), { version: 5, ecc: 'L' }), /too large/);
// All eight masks are valid symbols.
for (let mask = 0; mask < 8; mask++) {
    const text = paperUri(600, mask);
    assert.equal(decode(qr.encode(text, { version: 17, ecc: 'L', mask })).data, text, 'mask ' + mask);
}

// Known capacities (ISO/IEC 18004 table 7, byte mode).
assert.equal(qr.byteCapacity(1, 'L'), 17);
assert.equal(qr.byteCapacity(13, 'Q'), 241);
assert.equal(qr.byteCapacity(20, 'Q'), 482);
assert.equal(qr.byteCapacity(40, 'L'), 2953);
assert.equal(qr.byteCapacity(40, 'H'), 1273);

// Paper size rule: up to v20 at ECC Q keeps the logo look; beyond, smallest ECC L.
const plan482 = qr.paperPlan(paperUri(482));
assert.deepEqual([plan482.version, plan482.ecc, plan482.logo], [20, 'Q', true]);
const plan483 = qr.paperPlan(paperUri(483));
assert.deepEqual([plan483.ecc, plan483.logo], ['L', false]);
assert.equal(plan483.version, 15, 'larger links use the smallest ECC L version that fits');
const plan2953 = qr.paperPlan(paperUri(2953));
assert.deepEqual([plan2953.version, plan2953.ecc, plan2953.modules], [40, 'L', 185]);
assert.throws(() => qr.paperPlan(paperUri(2954)), /too large/);

// The logo-sized hole (15.5% + clearance) must stay inside ECC Q's budget.
for (const length of [260, 320, 400, 482]) {
    const text = paperUri(length, length);
    const plan = qr.paperPlan(text);
    const symbol = qr.encode(text, { version: plan.version, ecc: plan.ecc });
    const modules = plan.modules;
    const clear = modules * 0.155 * 1.18;
    const min = (modules - clear) / 2;
    const max = (modules + clear) / 2;
    for (let y = 0; y < symbol.size; y++) {
        for (let x = 0; x < symbol.size; x++) {
            const cx = x + 4 + 0.5;
            const cy = y + 4 + 0.5;
            if (cx >= min && cx <= max && cy >= min && cy <= max) symbol.modules[y][x] = false;
        }
    }
    assert.equal(decode(symbol, 4).data, text, length + '-char paper link must decode under the logo');
}

// Dense paper symbols draw whole, integer cells and decode from the drawn pixels.
for (const length of [600, 1300, 2200, 2953]) {
    const text = paperUri(length, 3);
    const canvas = recordingCanvas();
    canvas.style = {};
    const plan = qr.renderPaperCanvas(canvas, text, { cssWidth: 388, devicePixelRatio: 1 });
    const cell = Math.floor(388 / plan.modules);
    assert.equal(canvas.width, cell * plan.modules, 'backing store is a whole number of cells');
    assert.equal(canvas.style.width, canvas.width + 'px', 'shown 1:1 in device pixels at DPR 1');
    assert(canvas.rects.every(r => r.every(Number.isInteger)), 'no fractional module edges');
    assert(!canvas.ops.some(op => op[0] === 'quadraticCurveTo'), 'dense symbols use square modules and no logo');
    const n = canvas.width;
    const data = new Uint8ClampedArray(n * n * 4).fill(255);
    canvas.rects.slice(1).forEach(([x, y, w, h]) => {
        for (let yy = y; yy < y + h; yy++) {
            for (let xx = x; xx < x + w; xx++) {
                const i = (yy * n + xx) * 4;
                data[i] = data[i + 1] = data[i + 2] = 0;
            }
        }
    });
    const result = jsQR(data, n, n);
    assert(result && result.data === text, length + '-char paper link must decode from the rendered canvas');
}
// Short links keep the logo but draw square modules too (jsQR misreads rounded ones).
const logoCanvas = recordingCanvas();
const logoPlan = qr.renderPaperCanvas(logoCanvas, paperUri(300), { pixelsPerModule: 8 });
assert(logoPlan.logo && logoCanvas.rects.length > 500, 'logo-sized paper symbols draw square cells');
assert(logoCanvas.rects.every(r => r.every(Number.isInteger)));

// Scan Photo: the contact card's inset modules need the gap-closing retry.
function gappedContact(text) {
    const symbol = qr.encode(text, { version: 13, ecc: 'Q', mask: 0 });
    const n = 900;
    const cell = n / (symbol.size + 8);
    const data = new Uint8ClampedArray(n * n * 4).fill(255);
    for (let y = 0; y < symbol.size; y++) {
        for (let x = 0; x < symbol.size; x++) {
            if (!symbol.modules[y][x]) continue;
            for (let yy = Math.round((y + 4.08) * cell); yy < Math.round((y + 4.92) * cell); yy++) {
                for (let xx = Math.round((x + 4.08) * cell); xx < Math.round((x + 4.92) * cell); xx++) {
                    const i = (yy * n + xx) * 4;
                    data[i] = data[i + 1] = data[i + 2] = 17;
                }
            }
        }
    }
    return data;
}
for (const text of ['RSCP1:' + 'Ab9_'.repeat(55), 'RSCP1:short']) {
    const data = gappedContact(text);
    assert.equal(jsQR(data, 900, 900), null, 'plain jsQR cannot read inset modules');
    assert.equal(qr.decodeImageData(data, 900, 900).data, text, 'the closing retry reads the contact card');
}
assert(contactSource.includes("status.textContent = checkingText;\n                        var afterPaint = window.requestAnimationFrame") &&
    contactSource.includes('finish(decodeRoundedQr(image.data, width, height));'),
    'Scan Photo shows "Checking QR…" before the slow gap-closing retry');

const retina = recordingCanvas();
retina.style = {};
qr.renderPaperCanvas(retina, paperUri(2953), { cssWidth: 386, devicePixelRatio: 2 });
assert.deepEqual([retina.width, retina.style.width, retina.style.height], [740, '370px', '370px'],
    'v40 at DPR 2: 4 device px per module, 370 CSS px');

const exported = recordingCanvas();
qr.renderPaperCanvas(exported, paperUri(2953), { pixelsPerModule: 8 });
assert.equal(exported.width, 1480, 'v40 exports at 8 px per module');

assert(contactSource.includes("return QrEncode(text, { version: 13, ecc: 'Q', mask: 0 });"),
    'the contact card pins its symbol');
const vendor = fs.readFileSync(path.join(js, 'vendor', 'jsQR.js'), 'utf8');
assert(vendor.includes('alignmentPatternCenters: [6, 30, 54, 78, 102]'),
    'jsQR needs the corrected version 23 alignment table for Scan Photo');

console.log('QR encoder: contact card golden, v1-40 x L/M/Q/H decode, paper size rule passed');
