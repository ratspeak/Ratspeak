(function() {
    var CONTACT_QR_FILE = 'ratspeak-contact-card.png';
    var LEGACY_QR_FILE = 'ratspeak-contact-legacy.png';
    var CONTACT_SCAN_MAX_IMAGE_DIMENSION = 1800;
    var activeContactAddDial = null;

    function contactScanDiag(step, detail) {
        if (!window.RS || typeof RS.diag !== 'function') return;
        try {
            RS.diag('info', '[contact-scan] ' + step, detail || {});
        } catch (_) {}
    }

    function iconSvg(name) {
        if (name === 'qr') {
            return '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="3" width="7" height="7"/><rect x="14" y="3" width="7" height="7"/><rect x="3" y="14" width="7" height="7"/><path d="M14 14h3v3h-3z"/><path d="M19 14h2"/><path d="M14 21h7v-2"/><path d="M19 17h2"/></svg>';
        }
        if (name === 'scan') {
            return '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M7 3H5a2 2 0 0 0-2 2v2"/><path d="M17 3h2a2 2 0 0 1 2 2v2"/><path d="M21 17v2a2 2 0 0 1-2 2h-2"/><path d="M7 21H5a2 2 0 0 1-2-2v-2"/><path d="M7 12h10"/></svg>';
        }
        if (name === 'image') {
            return '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="3" width="18" height="18" rx="2"/><circle cx="8.5" cy="8.5" r="1.5"/><path d="m21 15-5-5L5 21"/></svg>';
        }
        if (name === 'copy') {
            return '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2"/><rect x="2" y="2" width="13" height="13" rx="2"/></svg>';
        }
        if (name === 'share') {
            return '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="18" cy="5" r="3"/><circle cx="6" cy="12" r="3"/><circle cx="18" cy="19" r="3"/><path d="M8.59 13.51l6.82 3.98"/><path d="M15.41 6.51 8.59 10.49"/></svg>';
        }
        if (name === 'address') {
            return '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="9" cy="7" r="4"/><path d="M15 19a6 6 0 0 0-12 0"/><path d="M19 8v6"/><path d="M22 11h-6"/></svg>';
        }
        return '';
    }

    function copyText(value, label) {
        if (!value) return Promise.resolve(false);
        return RS.copyText(value).then(function(ok) {
            if (ok && typeof showCopyConfirmationToast === 'function') showCopyConfirmationToast(label || 'Value');
            return ok;
        });
    }

    function shortValue(value, front, back) {
        if (!value) return '';
        front = front || 10;
        back = back || 6;
        if (typeof shortHash === 'function') return shortHash(value, front, back);
        if (value.length <= front + back + 1) return value;
        return value.substring(0, front) + '...' + value.substring(value.length - back);
    }

    function safeFileBase(name) {
        var cleaned = (name || 'ratspeak-contact').toLowerCase()
            .replace(/[^a-z0-9]+/g, '-')
            .replace(/^-+|-+$/g, '')
            .substring(0, 40);
        return cleaned || 'ratspeak-contact';
    }

    function textBytes(text) {
        if (window.TextEncoder) return new TextEncoder().encode(text);
        var encoded = unescape(encodeURIComponent(text));
        var out = new Uint8Array(encoded.length);
        for (var i = 0; i < encoded.length; i++) out[i] = encoded.charCodeAt(i);
        return out;
    }

    // ISO/IEC 18004 block structure, indexed [ecc][version]; index 0 unused.
    var QR_ECC_ORDER = ['L', 'M', 'Q', 'H'];
    var QR_ECC_FORMAT_BITS = { L: 1, M: 0, Q: 3, H: 2 };
    var QR_ECC_CODEWORDS_PER_BLOCK = {
        L: [-1, 7, 10, 15, 20, 26, 18, 20, 24, 30, 18, 20, 24, 26, 30, 22, 24, 28, 30, 28, 28, 28, 28, 30, 30, 26, 28, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30],
        M: [-1, 10, 16, 26, 18, 24, 16, 18, 22, 22, 26, 30, 22, 22, 24, 24, 28, 28, 26, 26, 26, 26, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28],
        Q: [-1, 13, 22, 18, 26, 18, 24, 18, 22, 20, 24, 28, 26, 24, 20, 30, 24, 28, 28, 26, 30, 28, 30, 30, 30, 30, 28, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30],
        H: [-1, 17, 28, 22, 16, 22, 28, 26, 26, 24, 28, 24, 28, 22, 24, 24, 30, 28, 28, 26, 28, 30, 24, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30],
    };
    var QR_ECC_BLOCKS = {
        L: [-1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 4, 4, 4, 4, 4, 6, 6, 6, 6, 7, 8, 8, 9, 9, 10, 12, 12, 12, 13, 14, 15, 16, 17, 18, 19, 19, 20, 21, 22, 24, 25],
        M: [-1, 1, 1, 1, 2, 2, 4, 4, 4, 5, 5, 5, 8, 9, 9, 10, 10, 11, 13, 14, 16, 17, 17, 18, 20, 21, 23, 25, 26, 28, 29, 31, 33, 35, 37, 38, 40, 43, 45, 47, 49],
        Q: [-1, 1, 1, 2, 2, 4, 4, 6, 6, 8, 8, 8, 10, 12, 16, 12, 17, 16, 18, 21, 20, 23, 23, 25, 27, 29, 34, 34, 35, 38, 40, 43, 45, 48, 51, 53, 56, 59, 62, 65, 68],
        H: [-1, 1, 1, 2, 4, 4, 4, 5, 6, 8, 8, 11, 11, 16, 16, 18, 16, 19, 21, 25, 25, 25, 34, 30, 32, 35, 37, 40, 42, 45, 48, 51, 54, 57, 60, 63, 66, 70, 74, 77, 81],
    };

    function qrRawDataModules(version) {
        var result = (16 * version + 128) * version + 64;
        if (version >= 2) {
            var numAlign = Math.floor(version / 7) + 2;
            result -= (25 * numAlign - 10) * numAlign - 55;
            if (version >= 7) result -= 36;
        }
        return result;
    }

    function qrDataCodewords(version, ecc) {
        return Math.floor(qrRawDataModules(version) / 8) -
            QR_ECC_CODEWORDS_PER_BLOCK[ecc][version] * QR_ECC_BLOCKS[ecc][version];
    }

    function qrByteCapacity(version, ecc) {
        return Math.floor((qrDataCodewords(version, ecc) * 8 - 4 - (version >= 10 ? 16 : 8)) / 8);
    }

    function qrAlignmentPositions(version) {
        if (version === 1) return [];
        var size = version * 4 + 17;
        var numAlign = Math.floor(version / 7) + 2;
        var step = version === 32 ? 26 : Math.ceil((version * 4 + 4) / (numAlign * 2 - 2)) * 2;
        var result = [6];
        for (var pos = size - 7; result.length < numAlign; pos -= step) result.splice(1, 0, pos);
        return result;
    }

    // Smallest version (within maxVersion) holding the UTF-8 bytes at this ECC, or 0.
    function qrFitVersion(byteLength, ecc, maxVersion) {
        for (var v = 1; v <= (maxVersion || 40); v++) {
            if (qrByteCapacity(v, ecc) >= byteLength) return v;
        }
        return 0;
    }

    function qrMaskBit(mask, x, y) {
        switch (mask) {
            case 0: return (x + y) % 2 === 0;
            case 1: return y % 2 === 0;
            case 2: return x % 3 === 0;
            case 3: return (x + y) % 3 === 0;
            case 4: return (Math.floor(x / 3) + Math.floor(y / 2)) % 2 === 0;
            case 5: return x * y % 2 + x * y % 3 === 0;
            case 6: return (x * y % 2 + x * y % 3) % 2 === 0;
            default: return ((x + y) % 2 + x * y % 3) % 2 === 0;
        }
    }

    // Byte-mode QR. Omitting `mask` selects the lowest-penalty mask.
    function QrEncode(text, opts) {
        var VERSION = opts.version;
        var ECC = opts.ecc;
        var SIZE = VERSION * 4 + 17;
        var ALIGNMENT_POSITIONS = qrAlignmentPositions(VERSION);
        var ERROR_CORRECTION_FORMAT_BITS = QR_ECC_FORMAT_BITS[ECC];
        var BYTE_COUNT_BITS = VERSION >= 10 ? 16 : 8;
        var NUM_BLOCKS = QR_ECC_BLOCKS[ECC][VERSION];
        var ECC_CODEWORDS_PER_BLOCK = QR_ECC_CODEWORDS_PER_BLOCK[ECC][VERSION];
        var DATA_CODEWORDS = qrDataCodewords(VERSION, ECC);
        var RAW_CODEWORDS = Math.floor(qrRawDataModules(VERSION) / 8);
        var SHORT_BLOCK_LEN = Math.floor(RAW_CODEWORDS / NUM_BLOCKS);
        var NUM_SHORT_BLOCKS = NUM_BLOCKS - RAW_CODEWORDS % NUM_BLOCKS;
        var DATA_BLOCK_SIZES = [];
        for (var bi = 0; bi < NUM_BLOCKS; bi++) {
            DATA_BLOCK_SIZES.push(SHORT_BLOCK_LEN - ECC_CODEWORDS_PER_BLOCK + (bi < NUM_SHORT_BLOCKS ? 0 : 1));
        }
        var modules = [];
        var functionModules = [];
        for (var y = 0; y < SIZE; y++) {
            modules[y] = [];
            functionModules[y] = [];
            for (var x = 0; x < SIZE; x++) {
                modules[y][x] = false;
                functionModules[y][x] = false;
            }
        }

        function setFunction(x, y, black) {
            if (x < 0 || y < 0 || x >= SIZE || y >= SIZE) return;
            modules[y][x] = !!black;
            functionModules[y][x] = true;
        }

        function drawFinder(cx, cy) {
            for (var dy = -4; dy <= 4; dy++) {
                for (var dx = -4; dx <= 4; dx++) {
                    var dist = Math.max(Math.abs(dx), Math.abs(dy));
                    setFunction(cx + dx, cy + dy, dist !== 2 && dist !== 4);
                }
            }
        }

        function drawAlignment(cx, cy) {
            for (var dy = -2; dy <= 2; dy++) {
                for (var dx = -2; dx <= 2; dx++) {
                    setFunction(cx + dx, cy + dy, Math.max(Math.abs(dx), Math.abs(dy)) !== 1);
                }
            }
        }

        function drawFunctionPatterns() {
            drawFinder(3, 3);
            drawFinder(SIZE - 4, 3);
            drawFinder(3, SIZE - 4);
            for (var i = 8; i < SIZE - 8; i++) {
                setFunction(6, i, i % 2 === 0);
                setFunction(i, 6, i % 2 === 0);
            }
            ALIGNMENT_POSITIONS.forEach(function(x) {
                ALIGNMENT_POSITIONS.forEach(function(y) {
                    if ((x === 6 && y === 6) || (x === SIZE - 7 && y === 6) || (x === 6 && y === SIZE - 7)) return;
                    drawAlignment(x, y);
                });
            });
            setFunction(8, 4 * VERSION + 9, true);
            if (VERSION >= 7) drawVersionBits();
            drawFormatBits(0);
        }

        function drawFormatBits(mask) {
            var data = (ERROR_CORRECTION_FORMAT_BITS << 3) | mask;
            var rem = data;
            for (var i = 0; i < 10; i++) {
                rem = (rem << 1) ^ (((rem >>> 9) & 1) ? 0x537 : 0);
            }
            var bits = ((data << 10) | rem) ^ 0x5412;
            for (var j = 0; j <= 5; j++) setFunction(8, j, ((bits >>> j) & 1) !== 0);
            setFunction(8, 7, ((bits >>> 6) & 1) !== 0);
            setFunction(8, 8, ((bits >>> 7) & 1) !== 0);
            setFunction(7, 8, ((bits >>> 8) & 1) !== 0);
            for (var k = 9; k < 15; k++) setFunction(14 - k, 8, ((bits >>> k) & 1) !== 0);
            for (var a = 0; a < 8; a++) setFunction(SIZE - 1 - a, 8, ((bits >>> a) & 1) !== 0);
            for (var b = 8; b < 15; b++) setFunction(8, SIZE - 15 + b, ((bits >>> b) & 1) !== 0);
            setFunction(8, SIZE - 8, true);
        }

        function drawVersionBits() {
            var rem = VERSION;
            for (var i = 0; i < 12; i++) {
                rem = (rem << 1) ^ (((rem >>> 11) & 1) ? 0x1f25 : 0);
            }
            var bits = (VERSION << 12) | rem;
            for (var j = 0; j < 18; j++) {
                var bit = ((bits >>> j) & 1) !== 0;
                var a = SIZE - 11 + (j % 3);
                var b = Math.floor(j / 3);
                setFunction(a, b, bit);
                setFunction(b, a, bit);
            }
        }

        function appendBits(arr, value, len) {
            for (var i = len - 1; i >= 0; i--) arr.push((value >>> i) & 1);
        }

        function gfMultiply(x, y) {
            var z = 0;
            while (y > 0) {
                if (y & 1) z ^= x;
                x <<= 1;
                if (x & 0x100) x ^= 0x11d;
                y >>>= 1;
            }
            return z & 0xff;
        }

        function reedSolomonDivisor(degree) {
            var result = [];
            for (var z = 0; z < degree - 1; z++) result.push(0);
            result.push(1);
            var root = 1;
            for (var i = 0; i < degree; i++) {
                for (var j = 0; j < result.length; j++) {
                    result[j] = gfMultiply(result[j], root);
                    if (j + 1 < result.length) result[j] ^= result[j + 1];
                }
                root = gfMultiply(root, 2);
            }
            return result;
        }

        function reedSolomonRemainder(data, divisor) {
            var result = [];
            for (var i = 0; i < divisor.length; i++) result.push(0);
            data.forEach(function(b) {
                var factor = b ^ result.shift();
                result.push(0);
                for (var j = 0; j < result.length; j++) {
                    result[j] ^= gfMultiply(divisor[j], factor);
                }
            });
            return result;
        }

        function encodeCodewords(value) {
            var bytes = textBytes(value);
            var maxBytePayload = Math.floor((DATA_CODEWORDS * 8 - 4 - BYTE_COUNT_BITS) / 8);
            if (bytes.length > maxBytePayload) {
                throw new Error('Share payload is too large for the QR layout');
            }
            var bits = [];
            appendBits(bits, 0x4, 4);
            appendBits(bits, bytes.length, BYTE_COUNT_BITS);
            for (var i = 0; i < bytes.length; i++) appendBits(bits, bytes[i], 8);
            var capacityBits = DATA_CODEWORDS * 8;
            var terminator = Math.min(4, capacityBits - bits.length);
            appendBits(bits, 0, terminator);
            while (bits.length % 8 !== 0) bits.push(0);

            var data = [];
            for (var j = 0; j < bits.length; j += 8) {
                var byte = 0;
                for (var k = 0; k < 8; k++) byte = (byte << 1) | bits[j + k];
                data.push(byte);
            }
            for (var pad = 0; data.length < DATA_CODEWORDS; pad++) {
                data.push((pad % 2 === 0) ? 0xec : 0x11);
            }

            var divisor = reedSolomonDivisor(ECC_CODEWORDS_PER_BLOCK);
            var blocks = [];
            var offset = 0;
            for (var block = 0; block < DATA_BLOCK_SIZES.length; block++) {
                var blockSize = DATA_BLOCK_SIZES[block];
                var part = data.slice(offset, offset + blockSize);
                offset += blockSize;
                blocks.push({ data: part, ecc: reedSolomonRemainder(part, divisor) });
            }

            var out = [];
            var maxBlockSize = Math.max.apply(null, DATA_BLOCK_SIZES);
            for (var x = 0; x < maxBlockSize; x++) {
                for (var b = 0; b < blocks.length; b++) {
                    if (x < blocks[b].data.length) out.push(blocks[b].data[x]);
                }
            }
            for (var e = 0; e < ECC_CODEWORDS_PER_BLOCK; e++) {
                for (var c = 0; c < blocks.length; c++) out.push(blocks[c].ecc[e]);
            }
            return out;
        }

        function drawCodewords(codewords) {
            var bits = [];
            codewords.forEach(function(cw) {
                for (var i = 7; i >= 0; i--) bits.push((cw >>> i) & 1);
            });
            var bitIndex = 0;
            var upward = true;
            for (var right = SIZE - 1; right >= 1; right -= 2) {
                if (right === 6) right--;
                for (var vert = 0; vert < SIZE; vert++) {
                    var y = upward ? SIZE - 1 - vert : vert;
                    for (var j = 0; j < 2; j++) {
                        var x = right - j;
                        if (functionModules[y][x]) continue;
                        modules[y][x] = !!(bitIndex < bits.length ? bits[bitIndex++] : 0);
                    }
                }
                upward = !upward;
            }
        }

        function applyMask(mask) {
            for (var y = 0; y < SIZE; y++) {
                for (var x = 0; x < SIZE; x++) {
                    if (!functionModules[y][x] && qrMaskBit(mask, x, y)) modules[y][x] = !modules[y][x];
                }
            }
        }

        function finderLikeRuns(run) {
            var n = run[1];
            var core = n > 0 && run[2] === n && run[3] === n * 3 && run[4] === n && run[5] === n;
            return (core && run[0] >= n * 4 && run[6] >= n ? 1 : 0) +
                (core && run[6] >= n * 4 && run[0] >= n ? 1 : 0);
        }

        function penaltyLine(get) {
            var score = 0;
            var runColor = false;
            var runLen = 0;
            var history = [0, 0, 0, 0, 0, 0, 0];
            function push(len) {
                if (history[0] === 0) len += SIZE;
                history.pop();
                history.unshift(len);
            }
            for (var i = 0; i < SIZE; i++) {
                if (get(i) === runColor) {
                    runLen++;
                    if (runLen === 5) score += 3;
                    else if (runLen > 5) score++;
                } else {
                    push(runLen);
                    if (!runColor) score += finderLikeRuns(history) * 40;
                    runColor = get(i);
                    runLen = 1;
                }
            }
            if (runColor) {
                push(runLen);
                runLen = 0;
            }
            runLen += SIZE;
            push(runLen);
            return score + finderLikeRuns(history) * 40;
        }

        function penaltyScore() {
            var score = 0;
            var dark = 0;
            for (var y = 0; y < SIZE; y++) {
                score += penaltyLine(function(i) { return modules[y][i]; });
                score += penaltyLine(function(i) { return modules[i][y]; });
            }
            for (var by = 0; by < SIZE - 1; by++) {
                for (var bx = 0; bx < SIZE - 1; bx++) {
                    var c = modules[by][bx];
                    if (c === modules[by][bx + 1] && c === modules[by + 1][bx] && c === modules[by + 1][bx + 1]) score += 3;
                }
            }
            for (var ry = 0; ry < SIZE; ry++) {
                for (var rx = 0; rx < SIZE; rx++) if (modules[ry][rx]) dark++;
            }
            var total = SIZE * SIZE;
            score += (Math.ceil(Math.abs(dark * 20 - total * 10) / total) - 1) * 10;
            return score;
        }

        drawFunctionPatterns();
        drawCodewords(encodeCodewords(text));
        var mask = opts.mask;
        if (mask === undefined || mask === null) {
            var best = Infinity;
            for (var m = 0; m < 8; m++) {
                applyMask(m);
                drawFormatBits(m);
                var penalty = penaltyScore();
                if (penalty < best) {
                    best = penalty;
                    mask = m;
                }
                applyMask(m);
            }
        }
        applyMask(mask);
        drawFormatBits(mask);
        return { size: SIZE, modules: modules, version: VERSION, ecc: ECC, mask: mask };
    }

    // Fixed v13-Q, mask 0: the contact card symbol is part of its shared look.
    function QrContactCard(text) {
        return QrEncode(text, { version: 13, ecc: 'Q', mask: 0 });
    }

    function roundRect(ctx, x, y, w, h, r) {
        r = Math.min(r, w / 2, h / 2);
        ctx.beginPath();
        ctx.moveTo(x + r, y);
        ctx.lineTo(x + w - r, y);
        ctx.quadraticCurveTo(x + w, y, x + w, y + r);
        ctx.lineTo(x + w, y + h - r);
        ctx.quadraticCurveTo(x + w, y + h, x + w - r, y + h);
        ctx.lineTo(x + r, y + h);
        ctx.quadraticCurveTo(x, y + h, x, y + h - r);
        ctx.lineTo(x, y + r);
        ctx.quadraticCurveTo(x, y, x + r, y);
        ctx.closePath();
    }

    var RATSPEAK_MARK_PATHS = [
        `M327.968170,501.158813
	C314.612823,508.030457 301.568787,514.718689 288.564667,521.483704
	C283.906769,523.906799 279.202484,524.608093 275.045868,520.980713
	C271.050140,517.493652 271.258057,512.871216 272.630554,508.048492
	C275.226746,498.926086 277.457489,489.699310 280.078735,480.584534
	C281.032684,477.267426 280.404602,475.510132 277.138275,473.917603
	C255.839752,463.533234 245.614563,446.289917 245.664932,422.656921
	C245.753387,381.157562 245.548752,339.657227 245.748459,298.158630
	C245.856934,275.621063 256.634552,259.571350 276.933105,249.976730
	C283.680115,246.787582 291.072327,246.010666 298.402832,245.996140
	C355.735321,245.882507 413.068329,245.826920 470.400726,245.953217
	C495.215057,246.007874 516.635620,265.694244 519.515930,290.025574
	C520.601440,299.195160 520.117065,308.321594 520.208130,317.464325
	C520.367493,333.462463 520.288940,349.463715 520.204651,365.463074
	C520.173462,371.381287 518.885925,372.034637 513.767273,369.133392
	C498.898499,360.705811 498.908051,360.705780 498.895477,343.646637
	C498.884583,328.813538 498.917877,313.979889 498.806915,299.147430
	C498.661743,279.741821 486.175995,267.275574 466.771118,267.263916
	C410.938477,267.230347 355.105713,267.234070 299.273132,267.318817
	C279.539673,267.348785 267.321625,279.364319 267.234802,299.048218
	C267.051025,340.713257 267.036682,382.379395 267.091003,424.044922
	C267.113922,441.620758 277.671661,453.848572 295.433075,456.579468
	C310.833984,458.947418 307.835663,458.978485 304.513245,471.318420
	C303.301056,475.820587 302.049133,480.312103 300.827454,484.811737
	C300.352844,486.559753 299.660675,488.316254 300.922791,490.332825
	C304.477539,490.202850 307.243988,487.866058 310.306793,486.457275
	C314.992493,484.302002 319.612335,481.945374 324.042511,479.308594
	C328.778992,476.489532 333.171173,476.591064 338.227417,478.730042
	C355.379700,485.986206 373.439697,488.836792 391.953949,487.719757
	C430.976227,485.365417 461.990295,468.126495 484.598419,436.222687
	C489.789673,428.896973 493.712006,420.739441 496.427124,412.077515
	C497.173920,409.695099 496.943207,408.002960 494.736664,406.520660
	C489.598328,403.068848 487.334564,398.047638 487.697083,391.962555
	C487.911255,388.368195 486.577209,386.133850 483.493866,384.241180
	C461.578094,370.788574 438.601746,360.130676 412.766998,356.866028
	C403.647125,355.713593 394.491638,355.670685 385.352783,356.729248
	C376.092834,357.801788 372.642639,358.017365 369.743683,347.737213
	C368.119751,341.978455 365.708679,336.467560 362.237030,331.471436
	C356.277893,322.895477 346.678650,319.018188 337.450958,321.631073
	C328.924500,324.045410 321.828003,333.468597 321.315216,343.136169
	C320.632874,356.000641 327.026306,371.922455 346.664215,374.878113
	C350.070923,375.390869 352.587311,376.980225 353.164948,380.743591
	C353.696350,384.205414 352.023376,386.530762 349.328644,388.251556
	C347.399170,389.483704 345.130554,389.393860 343.003998,389.008881
	C325.701508,385.876862 313.839172,376.012024 308.730072,359.239349
	C303.682312,342.667816 306.623260,327.487061 319.940338,315.423248
	C336.414337,300.499634 361.240723,303.856476 373.402649,322.510590
	C376.135284,326.701935 378.695312,331.062408 380.191284,335.834625
	C381.411041,339.725739 383.562256,341.029144 387.539886,340.768951
	C406.495819,339.528900 425.148529,341.259247 443.288330,347.180908
	C469.205597,355.641449 492.633240,368.823700 514.465332,384.974976
	C521.504395,390.182495 521.016174,397.617035 519.753296,405.007080
	C515.097656,432.250000 501.282715,454.401276 481.015900,472.623566
	C460.482239,491.085724 436.542145,502.917938 409.196136,507.517273
	C385.769257,511.457428 362.774963,509.704773 340.176849,502.297943
	C336.390533,501.056946 332.583923,498.310944 327.968170,501.158813
z`,
        `M414.192688,335.384125
	C405.752502,335.933838 397.647217,334.864838 389.538544,335.841949
	C386.999908,336.147858 385.783569,334.056091 384.801819,332.068085
	C380.734650,323.832214 376.210083,315.948059 368.774628,310.165710
	C366.479370,308.380737 367.227142,307.004120 369.431213,305.623901
	C379.533966,299.297333 389.616943,299.216583 399.186523,306.485565
	C408.523376,313.577728 414.075836,323.092621 414.192688,335.384125
z`,
        `M417.092285,390.209412
	C410.664825,388.775787 407.339386,384.601837 408.099579,379.437195
	C408.811401,374.601044 413.308868,370.908020 418.309540,371.053436
	C423.116180,371.193207 426.941650,375.015869 427.266174,380.003448
	C427.630859,385.607910 424.177490,389.218689 417.092285,390.209412
z`,
        `M430.829620,418.712555
	C429.468231,419.233032 428.588959,420.254486 427.026154,419.591888
	C426.810333,417.599304 428.520233,416.826111 429.772278,416.066620
	C441.624939,408.876678 453.852112,402.563873 467.791992,400.418243
	C469.449615,400.163086 471.076508,399.823425 472.692566,400.525970
	C474.423706,401.278534 475.233917,402.656433 475.093933,404.493347
	C474.921997,406.748901 473.272125,407.602539 471.339203,407.794617
	C465.711426,408.353851 460.116180,409.053802 454.587433,410.298187
	C446.442780,412.131348 438.565125,414.683960 430.829620,418.712555
z`,
        `M448.634430,432.622101
	C455.166016,424.816742 461.543457,417.301025 470.213623,412.353058
	C473.172333,410.664520 477.325500,407.477448 479.700378,412.176666
	C482.088440,416.901886 476.907593,417.667999 473.973694,418.989105
	C464.900238,423.074677 457.385437,429.349976 449.847137,435.628479
	C448.674011,436.605560 447.946289,438.424225 445.793304,438.039856
	C445.617737,435.721130 447.392334,434.492981 448.634430,432.622101
z`,
    ];

    function drawOfficialRatspeakMark(ctx, cx, cy, size, color) {
        if (typeof Path2D === 'undefined') return false;
        var ok = true;
        ctx.save();
        try {
            var scale = size / 358;
            ctx.translate(cx - size / 2, cy - size / 2);
            ctx.scale(scale, scale);
            ctx.translate(-205, -205);
            ctx.fillStyle = color || '#0a0f2e';
            for (var i = 0; i < RATSPEAK_MARK_PATHS.length; i++) {
                ctx.fill(new Path2D(RATSPEAK_MARK_PATHS[i]));
            }
        } catch (_) {
            ok = false;
        }
        ctx.restore();
        return ok;
    }

    function drawFallbackRatspeakMark(ctx, cx, cy, size) {
        var w = size * 0.58;
        var h = size * 0.40;
        var x = cx - w / 2;
        var y = cy - h / 2;
        ctx.strokeStyle = '#0a0f2e';
        ctx.lineWidth = Math.max(3, size * 0.05);
        ctx.lineCap = 'round';
        ctx.lineJoin = 'round';
        ctx.beginPath();
        ctx.moveTo(x + w * 0.24, y + h * 0.96);
        ctx.quadraticCurveTo(x + w * 0.02, y + h * 0.72, x + w * 0.08, y + h * 0.42);
        ctx.quadraticCurveTo(x + w * 0.18, y - h * 0.08, x + w * 0.68, y + h * 0.05);
        ctx.quadraticCurveTo(x + w * 1.06, y + h * 0.16, x + w * 0.94, y + h * 0.56);
        ctx.quadraticCurveTo(x + w * 0.84, y + h * 0.9, x + w * 0.46, y + h * 0.82);
        ctx.lineTo(x + w * 0.24, y + h * 0.96);
        ctx.stroke();
    }

    function drawRatspeakLogo(ctx, cx, cy, size, surface) {
        ctx.save();
        ctx.shadowColor = 'rgba(10, 15, 46, 0.16)';
        ctx.shadowBlur = size * 0.14;
        ctx.shadowOffsetY = size * 0.035;
        roundRect(ctx, cx - size / 2, cy - size / 2, size, size, size * 0.18);
        ctx.fillStyle = surface || '#ffffff';
        ctx.fill();
        ctx.shadowColor = 'transparent';
        ctx.lineWidth = Math.max(3, size * 0.07);
        ctx.strokeStyle = '#0a0f2e';
        ctx.stroke();
        if (!drawOfficialRatspeakMark(ctx, cx, cy, size * 0.7, '#0a0f2e')) {
            drawFallbackRatspeakMark(ctx, cx, cy, size * 0.7);
        }
        ctx.restore();
    }

    var QR_QUIET_MODULES = 4;
    var PAPER_QR_EXPORT_PX_PER_MODULE = 8;

    function drawQrSymbol(canvas, qr, pixels, cell, logo, rounded) {
        var quiet = QR_QUIET_MODULES;
        canvas.width = pixels;
        canvas.height = pixels;
        var ctx = canvas.getContext('2d');
        var qrSurface = '#ffffff';
        ctx.fillStyle = qrSurface;
        ctx.fillRect(0, 0, pixels, pixels);
        var logoSize = pixels * 0.155;
        var logoClearSize = logoSize * 1.18;
        var logoClearMin = (pixels - logoClearSize) / 2;
        var logoClearMax = (pixels + logoClearSize) / 2;
        function moduleFallsBehindLogo(px, py) {
            var cx = px + cell / 2;
            var cy = py + cell / 2;
            return cx >= logoClearMin && cx <= logoClearMax && cy >= logoClearMin && cy <= logoClearMax;
        }
        ctx.fillStyle = '#11100e';
        for (var y = 0; y < qr.size; y++) {
            for (var x = 0; x < qr.size; x++) {
                if (!qr.modules[y][x]) continue;
                var px = (x + quiet) * cell;
                var py = (y + quiet) * cell;
                if (logo && moduleFallsBehindLogo(px, py)) continue;
                if (!rounded) {
                    ctx.fillRect(px, py, cell, cell);
                    continue;
                }
                roundRect(ctx, px + cell * 0.08, py + cell * 0.08, cell * 0.84, cell * 0.84, cell * 0.24);
                ctx.fill();
            }
        }
        if (!logo) return canvas;
        roundRect(ctx, (pixels - logoClearSize) / 2, (pixels - logoClearSize) / 2, logoClearSize, logoClearSize, logoClearSize * 0.18);
        ctx.fillStyle = qrSurface;
        ctx.fill();
        drawRatspeakLogo(ctx, pixels / 2, pixels / 2, logoSize, qrSurface);
        return canvas;
    }

    function renderQrCanvas(canvas, payload) {
        var qr = QrContactCard(payload);
        var pixels = 900;
        return drawQrSymbol(canvas, qr, pixels, pixels / (qr.size + QR_QUIET_MODULES * 2), true, true);
    }

    // The lxma:// code is for other apps' scanners: square modules, no logo,
    // and the smallest ECC M symbol so modules stay large. Columba encodes at M.
    function renderLegacyQrCanvas(canvas, payload) {
        var version = qrFitVersion(textBytes(payload).length, 'M', 40);
        if (!version) throw new Error('Legacy payload is too large for a QR code');
        var qr = QrEncode(payload, { version: version, ecc: 'M' });
        var modules = qr.size + QR_QUIET_MODULES * 2;
        var cell = Math.floor(900 / modules);
        return drawQrSymbol(canvas, qr, cell * modules, cell, false, false);
    }

    // Up to v20 at ECC Q keeps the centre logo; larger payloads use the smallest
    // ECC L symbol (upstream LXMF), which cannot absorb one. Paper modules are
    // always square: jsQR (Scan Photo) misreads rounded modules above v10.
    function paperQrPlan(text) {
        var length = textBytes(text).length;
        var version = qrFitVersion(length, 'Q', 20);
        if (version) return { version: version, ecc: 'Q', logo: true, modules: version * 4 + 17 + QR_QUIET_MODULES * 2 };
        version = qrFitVersion(length, 'L', 40);
        if (!version) throw new Error('Paper message is too large for a QR code');
        return { version: version, ecc: 'L', logo: false, modules: version * 4 + 17 + QR_QUIET_MODULES * 2 };
    }

    // Integer pixels per module: no fractional cells, so no anti-aliased edges.
    function renderPaperQrCanvas(canvas, payload, options) {
        options = options || {};
        var plan = paperQrPlan(payload);
        var qr = QrEncode(payload, { version: plan.version, ecc: plan.ecc });
        var cell = options.pixelsPerModule;
        if (!cell) {
            // Whole device pixels per module, shown 1:1: no resampling at any DPR.
            var cssWidth = options.cssWidth || canvas.clientWidth || 400;
            var dpr = options.devicePixelRatio || window.devicePixelRatio || 1;
            cell = Math.max(1, Math.floor(cssWidth * dpr / plan.modules));
            if (canvas.style) {
                canvas.style.width = canvas.style.height = (cell * plan.modules / dpr) + 'px';
            }
        }
        drawQrSymbol(canvas, qr, cell * plan.modules, cell, plan.logo, false);
        return plan;
    }

    // Morphological closing (dilate dark, then erode) on luminance. jsQR's
    // finder search needs solid runs, so rounded, gapped modules (contact card,
    // short paper links) only decode once the gaps between them are filled.
    function closeQrGaps(rgba, width, height, radius) {
        var n = width * height;
        var a = new Uint8ClampedArray(n);
        var b = new Uint8ClampedArray(n);
        for (var i = 0, j = 0; i < n; i++, j += 4) {
            a[i] = (rgba[j] * 299 + rgba[j + 1] * 587 + rgba[j + 2] * 114) / 1000;
        }
        function pass(src, dst, horizontal, takeMin) {
            for (var y = 0; y < height; y++) {
                for (var x = 0; x < width; x++) {
                    var best = takeMin ? 255 : 0;
                    for (var k = -radius; k <= radius; k++) {
                        var xx = horizontal ? x + k : x;
                        var yy = horizontal ? y : y + k;
                        if (xx < 0 || yy < 0 || xx >= width || yy >= height) continue;
                        var v = src[yy * width + xx];
                        if (takeMin ? v < best : v > best) best = v;
                    }
                    dst[y * width + x] = best;
                }
            }
        }
        pass(a, b, true, true);
        pass(b, a, false, true);
        pass(a, b, true, false);
        pass(b, a, false, false);
        var out = new Uint8ClampedArray(n * 4);
        for (var p = 0, q = 0; p < n; p++, q += 4) {
            out[q] = out[q + 1] = out[q + 2] = a[p];
            out[q + 3] = 255;
        }
        return out;
    }

    // Half-size box filter. Large rounded-module exports (the 900px card PNG)
    // only decode once a module spans a few pixels rather than a dozen.
    function halveRgba(rgba, width, height) {
        var w = width >> 1;
        var h = height >> 1;
        var out = new Uint8ClampedArray(w * h * 4);
        for (var y = 0; y < h; y++) {
            for (var x = 0; x < w; x++) {
                var o = (y * w + x) * 4;
                var p = (y * 2 * width + x * 2) * 4;
                var q = p + width * 4;
                for (var c = 0; c < 3; c++) {
                    out[o + c] = (rgba[p + c] + rgba[p + 4 + c] + rgba[q + c] + rgba[q + 4 + c]) >> 2;
                }
                out[o + 3] = 255;
            }
        }
        return { data: out, width: w, height: h };
    }

    function decodeRoundedQr(rgba, width, height) {
        if (typeof window.jsQR !== 'function') return null;
        var base = Math.max(1, Math.round(Math.max(width, height) / 900));
        var radii = [base, base * 2];
        for (var i = 0; i < radii.length; i++) {
            var code = window.jsQR(closeQrGaps(rgba, width, height, radii[i]), width, height, { inversionAttempts: 'dontInvert' });
            if (code && code.data) return code;
        }
        if (Math.max(width, height) <= 600) return null;
        var half = halveRgba(rgba, width, height);
        var direct = window.jsQR(half.data, half.width, half.height, { inversionAttempts: 'dontInvert' });
        return direct && direct.data ? direct : decodeRoundedQr(half.data, half.width, half.height);
    }

    function canvasBlob(canvas) {
        return new Promise(function(resolve, reject) {
            canvas.toBlob(function(blob) {
                if (blob) resolve(blob);
                else reject(new Error('Could not render QR image'));
            }, 'image/png');
        });
    }

    function saveQrBlob(blob, fileName, shareTitle) {
        shareTitle = shareTitle || 'Ratspeak Contact Card';
        if (window.File && navigator.canShare && navigator.share) {
            try {
                var file = new File([blob], fileName, { type: 'image/png' });
                if (navigator.canShare({ files: [file] })) {
                    return navigator.share({ files: [file], title: shareTitle })
                        .then(function() { return 'share'; });
                }
            } catch (_) {}
        }
        return blob.arrayBuffer().then(function(buf) {
            var bytes = new Uint8Array(buf);
            if (typeof saveBytesToUserFile === 'function') {
                return saveBytesToUserFile(bytes, fileName, 'image/png').then(function() { return 'file'; });
            }
            var url = URL.createObjectURL(blob);
            var a = document.createElement('a');
            a.href = url;
            a.download = fileName;
            a.style.display = 'none';
            document.body.appendChild(a);
            a.click();
            a.remove();
            setTimeout(function() { URL.revokeObjectURL(url); }, 60000);
            return 'download';
        });
    }

    function shareQrCanvas(canvas, fileName, shareTitle) {
        return canvasBlob(canvas).then(function(blob) {
            return saveQrBlob(blob, fileName, shareTitle);
        });
    }

    function closeSheet(overlay, sheet, onClose) {
        RS.sheetShell.dismiss({ overlay: overlay, sheet: sheet }, onClose);
    }

    function buildSheet(className) {
        var shell = RS.sheetShell.create({
            overlayClass: 'contact-card-overlay',
            sheetClass: className,
        });
        RS.sheetShell.present(shell);
        return shell;
    }

    function showIdentityShareScreen(identityHash) {
        if (document.querySelector('.contact-share-sheet.open')) return;
        var built = buildSheet('contact-share-sheet');
        var closed = false;
        var loadGeneration = 0;
        built.sheet.setAttribute('role', 'dialog');
        built.sheet.setAttribute('aria-modal', 'true');
        built.sheet.setAttribute('tabindex', '-1');
        built.sheet.addEventListener('keydown', function(e) {
            if (e.key === 'Escape') {
                e.stopPropagation();
                closeShareScreen();
            }
        });

        function closeShareScreen() {
            if (closed) return;
            closed = true;
            closeSheet(built.overlay, built.sheet);
        }

        function bindCloseButton() {
            var closeButton = built.sheet.querySelector('.contact-card-close');
            if (closeButton) closeButton.addEventListener('click', closeShareScreen);
        }

        function renderLoading() {
            built.sheet.setAttribute('aria-busy', 'true');
            built.sheet.innerHTML =
                '<div class="contact-card-topbar">' +
                    '<button class="contact-card-close" type="button" aria-label="Close">&times;</button>' +
                '</div>' +
                '<div class="contact-share-loading" role="status" aria-live="polite">' +
                    '<div class="contact-share-loading-qr" aria-hidden="true">' +
                        '<span class="loading-spinner"></span>' +
                    '</div>' +
                    '<div class="contact-share-loading-label">Preparing contact card&hellip;</div>' +
                '</div>';
            bindCloseButton();
        }

        function renderError(err, generation) {
            if (closed || generation !== loadGeneration) return;
            built.sheet.removeAttribute('aria-busy');
            var message = err && err.message ? err.message : 'Could not build contact card';
            built.sheet.innerHTML =
                '<div class="contact-card-topbar">' +
                    '<button class="contact-card-close" type="button" aria-label="Close">&times;</button>' +
                '</div>' +
                '<div class="contact-share-error" role="alert">' +
                    '<div class="contact-share-error-title">Contact card unavailable</div>' +
                    '<div class="contact-share-error-message">' + escapeHtml(message) + '</div>' +
                    '<button class="nr-btn contact-share-retry" type="button">Try again</button>' +
                '</div>';
            bindCloseButton();
            built.sheet.querySelector('.contact-share-retry').addEventListener('click', loadCard);
        }

        function renderCard(card, generation) {
            if (closed || generation !== loadGeneration) return;
            var name = card.display_name || 'Ratspeak Contact';
            var fileBase = safeFileBase(name);
            var formats = {
                ratspeak: {
                    subtitle: 'Ratspeak Contact Card',
                    hint: 'Scans in Ratspeak. Carries your name, address, and key.',
                    ariaLabel: 'Ratspeak contact card QR for ' + name,
                    file: fileBase + '-' + CONTACT_QR_FILE
                },
                legacy: {
                    subtitle: 'Legacy Contact QR',
                    hint: 'For Columba and other Reticulum apps. Address and key only, no name.',
                    ariaLabel: 'Legacy contact QR for ' + name,
                    file: fileBase + '-' + LEGACY_QR_FILE
                }
            };
            var format = 'ratspeak';
            var drawSequence = 0;
            built.sheet.removeAttribute('aria-busy');
            built.sheet.innerHTML =
                '<div class="contact-card-topbar">' +
                    '<button class="contact-card-close" type="button" aria-label="Close">&times;</button>' +
                '</div>' +
                '<div class="contact-share-identity">' +
                    '<div class="contact-share-avatar">' + (typeof identityAvatar === 'function' ? identityAvatar(card.lxmf_hash, 56) : '') + '</div>' +
                    '<div class="contact-share-name">' + escapeHtml(name) + '</div>' +
                    '<div class="contact-share-subtitle">' + escapeHtml(formats.ratspeak.subtitle) + '</div>' +
                '</div>' +
                '<div class="contact-share-qr-shell">' +
                    '<canvas class="contact-share-qr" aria-label="' + escapeHtml(formats.ratspeak.ariaLabel) + '"></canvas>' +
                '</div>' +
                '<div class="sheet-segmented-tabs contact-share-format" role="group" aria-label="QR format">' +
                    '<button type="button" class="contact-share-format-option active" data-format="ratspeak" aria-pressed="true">Ratspeak</button>' +
                    '<button type="button" class="contact-share-format-option" data-format="legacy" aria-pressed="false">Legacy</button>' +
                '</div>' +
                '<div class="contact-share-subtitle contact-share-format-hint">' + escapeHtml(formats.ratspeak.hint) + '</div>' +
                '<div class="contact-share-address-block">' +
                    '<div class="contact-share-address-label">LXMF Address</div>' +
                    '<div class="contact-share-address mono">' + escapeHtml(card.lxmf_hash || '') + '</div>' +
                '</div>' +
                '<div class="contact-share-actions">' +
                    '<button class="nr-btn contact-share-action" id="contact-copy-address">' + iconSvg('copy') + '<span>Copy</span></button>' +
                    '<button class="nr-btn contact-share-action" id="contact-share-qr" disabled aria-busy="true">' + iconSvg('qr') + '<span>Share QR</span></button>' +
                '</div>';

            var canvas = built.sheet.querySelector('canvas');
            var shareButton = built.sheet.querySelector('#contact-share-qr');
            var subtitle = built.sheet.querySelector('.contact-share-identity .contact-share-subtitle');
            var hint = built.sheet.querySelector('.contact-share-format-hint');
            var formatButtons = Array.prototype.slice.call(built.sheet.querySelectorAll('.contact-share-format-option'));
            bindCloseButton();
            built.sheet.querySelector('#contact-copy-address').addEventListener('click', function() {
                copyText(card.lxmf_hash, 'Address');
            });
            shareButton.addEventListener('click', function() {
                canvasBlob(canvas).then(function(blob) {
                    return saveQrBlob(blob, formats[format].file);
                }).then(function(method) {
                    if (method === 'share') showToast('QR code shared', 'toast-success', 2500);
                    else showToast('QR image saved', 'toast-success', 2500);
                }).catch(function(err) {
                    showToast(err && err.message ? err.message : 'Could not share QR', 'toast-error', 3000);
                });
            });

            // Let the ready sheet paint before the synchronous QR encoder runs.
            // Two animation frames guarantee one complete rendering opportunity.
            var nextFrame = window.requestAnimationFrame || function(callback) {
                return window.setTimeout(callback, 0);
            };
            function drawCurrentFormat() {
                var sequence = ++drawSequence;
                shareButton.disabled = true;
                shareButton.setAttribute('aria-busy', 'true');
                nextFrame(function() {
                    nextFrame(function() {
                        if (closed || generation !== loadGeneration || sequence !== drawSequence || !canvas.isConnected) return;
                        try {
                            if (format === 'legacy') renderLegacyQrCanvas(canvas, card.legacy_payload || '');
                            else renderQrCanvas(canvas, card.payload || '');
                            shareButton.disabled = false;
                            shareButton.removeAttribute('aria-busy');
                        } catch (err) {
                            shareButton.removeAttribute('aria-busy');
                            showToast(err && err.message ? err.message : 'Could not render QR', 'toast-error', 3000);
                        }
                    });
                });
            }
            function selectFormat(next) {
                if (next === format || !formats[next]) return;
                format = next;
                formatButtons.forEach(function(button) {
                    var selected = button.getAttribute('data-format') === format;
                    button.classList.toggle('active', selected);
                    button.setAttribute('aria-pressed', selected ? 'true' : 'false');
                });
                subtitle.textContent = formats[format].subtitle;
                hint.textContent = formats[format].hint;
                canvas.setAttribute('aria-label', formats[format].ariaLabel);
                drawCurrentFormat();
            }
            formatButtons.forEach(function(button) {
                button.addEventListener('click', function() {
                    selectFormat(button.getAttribute('data-format'));
                });
            });
            drawCurrentFormat();
            try { built.sheet.focus({ preventScroll: true }); } catch (_) { built.sheet.focus(); }
        }

        function loadCard() {
            if (closed) return;
            var generation = ++loadGeneration;
            renderLoading();
            RS.invoke('api_contact_card', { hashHex: identityHash || null }).then(function(card) {
                renderCard(card, generation);
            }).catch(function(err) {
                renderError(err, generation);
            });
        }

        built.overlay.addEventListener('click', function(e) {
            if (e.target === built.overlay) closeShareScreen();
        });
        built.sheet._ratspeakDismiss = closeShareScreen;
        loadCard();
    }

    function paperFileStamp(date) {
        function pad(n) { return (n < 10 ? '0' : '') + n; }
        return date.getFullYear() + pad(date.getMonth() + 1) + pad(date.getDate()) + '-' +
            pad(date.getHours()) + pad(date.getMinutes());
    }

    function paperShareFilesSupported() {
        if (!window.File || !navigator.canShare || !navigator.share) return false;
        try {
            return !!navigator.canShare({ files: [new File([], 'ratspeak-paper.png', { type: 'image/png' })] });
        } catch (_) {
            return false;
        }
    }

    // opts: { recipientHash, recipientName, uri } or { ..., loadUri: () => Promise<uri> }.
    function showPaperShareScreen(opts) {
        opts = opts || {};
        var built = buildSheet('contact-share-sheet paper-share-sheet');
        var closed = false;
        var loadGeneration = 0;
        var name = opts.recipientName || shortValue(opts.recipientHash || '', 8, 4) || 'Recipient';
        built.sheet.setAttribute('role', 'dialog');
        built.sheet.setAttribute('aria-modal', 'true');
        built.sheet.setAttribute('tabindex', '-1');

        function closeShareScreen() {
            if (closed) return;
            closed = true;
            closeSheet(built.overlay, built.sheet, opts.onClose);
        }

        function bindCloseButton() {
            var closeButton = built.sheet.querySelector('.contact-card-close');
            if (closeButton) closeButton.addEventListener('click', closeShareScreen);
            return closeButton;
        }

        function renderLoading() {
            built.sheet.setAttribute('aria-busy', 'true');
            built.sheet.setAttribute('aria-label', name);
            built.sheet.innerHTML =
                '<div class="contact-card-topbar">' +
                    '<button class="contact-card-close" type="button" aria-label="Close">&times;</button>' +
                '</div>' +
                '<div class="contact-share-loading" role="status" aria-live="polite">' +
                    '<div class="contact-share-loading-qr" aria-hidden="true">' +
                        '<span class="loading-spinner"></span>' +
                    '</div>' +
                    '<div class="contact-share-loading-label">Preparing paper message&hellip;</div>' +
                '</div>';
            bindCloseButton();
        }

        function renderError(err, generation) {
            if (closed || generation !== loadGeneration) return;
            built.sheet.removeAttribute('aria-busy');
            built.sheet.innerHTML =
                '<div class="contact-card-topbar">' +
                    '<button class="contact-card-close" type="button" aria-label="Close">&times;</button>' +
                '</div>' +
                '<div class="contact-share-error" role="alert">' +
                    '<div class="contact-share-error-title">Paper message unavailable</div>' +
                    '<div class="contact-share-error-message">' + escapeHtml(err && err.message ? err.message : 'Couldn\'t load the paper link.') + '</div>' +
                    '<button class="nr-btn contact-share-retry" type="button">Try again</button>' +
                '</div>';
            bindCloseButton();
            built.sheet.querySelector('.contact-share-retry').addEventListener('click', load);
        }

        function renderPaper(uri, generation) {
            if (closed || generation !== loadGeneration) return;
            var chars = uri.length.toLocaleString('en-US');
            var canShareFiles = paperShareFilesSupported();
            var fileName = 'ratspeak-paper-' + safeFileBase(name) + '-' + paperFileStamp(new Date()) + '.png';
            var nameId = 'paper-share-name-' + generation + '-' + Date.now();
            built.sheet.removeAttribute('aria-busy');
            built.sheet.removeAttribute('aria-label');
            built.sheet.setAttribute('aria-labelledby', nameId);
            built.sheet.innerHTML =
                '<div class="contact-card-topbar">' +
                    '<button class="contact-card-close" type="button" aria-label="Close">&times;</button>' +
                '</div>' +
                '<div class="contact-share-identity">' +
                    '<div class="contact-share-avatar">' + (typeof identityAvatar === 'function' && opts.recipientHash ? identityAvatar(opts.recipientHash, 56) : '') + '</div>' +
                    '<div class="contact-share-name" id="' + nameId + '">' + escapeHtml(name) + '</div>' +
                    '<div class="contact-share-subtitle">Ratspeak paper message</div>' +
                '</div>' +
                '<div class="contact-share-qr-shell">' +
                    '<canvas class="contact-share-qr" aria-label="' + escapeHtml('Paper message QR for ' + name) + '"></canvas>' +
                '</div>' +
                '<div class="contact-share-subtitle paper-share-helper">' + escapeHtml('Only ' + name + ' can read this.') + '</div>' +
                '<div class="contact-share-actions' + (canShareFiles ? '' : ' paper-share-actions-single') + '">' +
                    '<button class="nr-btn contact-share-action" type="button" id="paper-share-save" disabled aria-busy="true">' +
                        '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/></svg>' +
                        '<span>Save</span></button>' +
                    (canShareFiles
                        ? '<button class="nr-btn contact-share-action" type="button" id="paper-share-share" disabled aria-busy="true">' + iconSvg('qr') + '<span>Share</span></button>'
                        : '') +
                '</div>' +
                '<div class="contact-share-address-block paper-share-link-block">' +
                    '<div class="contact-share-address-label">Paper link · ' + chars + ' characters</div>' +
                    '<div class="paper-share-link-row">' +
                        '<div class="contact-share-address paper-share-link mono">' + escapeHtml(uri) + '</div>' +
                        '<button class="nr-btn nr-btn-ghost paper-share-copy" type="button" aria-label="Copy paper link">' + iconSvg('copy') + '</button>' +
                    '</div>' +
                '</div>';

            var canvas = built.sheet.querySelector('canvas');
            var saveButton = built.sheet.querySelector('#paper-share-save');
            var shareButton = built.sheet.querySelector('#paper-share-share');
            bindCloseButton();
            built.sheet.querySelector('.paper-share-copy').addEventListener('click', function() {
                copyText(uri, 'Paper link');
            });

            function exportBlob() {
                var exportCanvas = document.createElement('canvas');
                renderPaperQrCanvas(exportCanvas, uri, { pixelsPerModule: PAPER_QR_EXPORT_PX_PER_MODULE });
                return canvasBlob(exportCanvas);
            }

            saveButton.addEventListener('click', function() {
                exportBlob().then(function(blob) {
                    return blob.arrayBuffer();
                }).then(function(buf) {
                    if (typeof saveBytesToUserFile !== 'function') throw new Error('Saving isn\'t available in this build');
                    return saveBytesToUserFile(new Uint8Array(buf), fileName, 'image/png');
                }).then(function(result) {
                    if (result && result.status === 'cancelled') return;
                    if (result && result.status === 'failed') {
                        showToast('Couldn\'t save the QR image', 'toast-error', 3000);
                        return;
                    }
                    showToast('QR image saved', 'toast-success', 2500);
                }).catch(function(err) {
                    if (err && err.cancelled) return;
                    showToast(err && err.message ? err.message : 'Couldn\'t save the QR image', 'toast-error', 3000);
                });
            });
            if (shareButton) {
                shareButton.addEventListener('click', function() {
                    exportBlob().then(function(blob) {
                        var file = new File([blob], fileName, { type: 'image/png' });
                        return navigator.share({ files: [file], title: 'Ratspeak paper message' });
                    }).then(function() {
                        showToast('QR code shared', 'toast-success', 2500);
                    }).catch(function(err) {
                        if (err && err.name === 'AbortError') return;
                        showToast(err && err.message ? err.message : 'Couldn\'t share the QR code', 'toast-error', 3000);
                    });
                });
            }
            // Focus the dialog itself: no ring on Close, and Tab still starts there.
            try { built.sheet.focus({ preventScroll: true }); } catch (_) { built.sheet.focus(); }

            // Let the ready sheet paint before the synchronous QR encoder runs.
            var nextFrame = window.requestAnimationFrame || function(callback) {
                return window.setTimeout(callback, 0);
            };
            nextFrame(function() {
                nextFrame(function() {
                    if (closed || generation !== loadGeneration || !canvas.isConnected) return;
                    try {
                        renderPaperQrCanvas(canvas, uri);
                        [saveButton, shareButton].forEach(function(btn) {
                            if (!btn) return;
                            btn.disabled = false;
                            btn.removeAttribute('aria-busy');
                        });
                    } catch (err) {
                        [saveButton, shareButton].forEach(function(btn) {
                            if (btn) btn.removeAttribute('aria-busy');
                        });
                        showToast(err && err.message ? err.message : 'Couldn\'t draw the QR code', 'toast-error', 3000);
                    }
                });
            });
        }

        function load() {
            if (closed) return;
            var generation = ++loadGeneration;
            if (opts.uri) {
                renderPaper(opts.uri, generation);
                return;
            }
            renderLoading();
            Promise.resolve(typeof opts.loadUri === 'function' ? opts.loadUri() : null).then(function(uri) {
                if (!uri) throw new Error('Couldn\'t load the paper link.');
                opts.uri = uri;
                renderPaper(uri, generation);
            }).catch(function(err) {
                renderError(err, generation);
            });
        }

        built.overlay.addEventListener('click', function(e) {
            if (e.target === built.overlay) closeShareScreen();
        });
        built.sheet.addEventListener('keydown', function(e) {
            if (e.key === 'Escape') {
                e.stopPropagation();
                closeShareScreen();
            }
        });
        built.sheet._ratspeakDismiss = closeShareScreen;
        load();
        return { close: closeShareScreen };
    }

    function showScannedCardPreview(parent, payload, card, closeAll) {
        var legacy = !!card.legacy;
        var name = card.display_name || (legacy ? shortValue(card.lxmf_hash, 8, 4) : 'Unnamed Contact');
        var subtitle = legacy ? 'Legacy contact QR \u00b7 no name included'
            : (card.is_device ? 'Device card verified' : 'Contact card verified');
        var caps = Array.isArray(card.caps) ? card.caps : [];
        var deviceRow = card.is_device
            ? '<div class="contact-card-detail-row"><span>Device</span><code>' +
                escapeHtml(caps.indexOf('rsnode') !== -1 ? 'rsNode Repeater' : 'Device') + '</code></div>'
            : '';
        parent.innerHTML =
            '<div class="contact-scan-success">' +
                '<div class="contact-scan-avatar">' + (typeof identityAvatar === 'function' ? identityAvatar(card.lxmf_hash, 72) : '') + '</div>' +
                '<div class="contact-scan-name' + (legacy && !card.display_name ? ' mono' : '') + '">' + escapeHtml(name) + '</div>' +
                '<div class="contact-scan-subtitle">' + subtitle + '</div>' +
                '<div class="contact-card-detail-list">' + deviceRow +
                    '<div class="contact-card-detail-row"><span>LXMF Address</span><code>' + escapeHtml(shortValue(card.lxmf_hash, 12, 8)) + '</code></div>' +
                    '<div class="contact-card-detail-row"><span>Identity Hash</span><code>' + escapeHtml(shortValue(card.identity_hash, 12, 8)) + '</code></div>' +
                    '<div class="contact-card-detail-row"><span>Public Identity Key</span><code>' + escapeHtml(shortValue(card.public_key, 12, 10)) + '</code></div>' +
                '</div>' +
                '<div class="contact-scan-actions">' +
                    '<button class="nr-btn nr-btn-ghost" id="contact-scan-cancel">Cancel</button>' +
                    '<button class="nr-btn nr-btn-primary" id="contact-scan-add">Add</button>' +
                '</div>' +
            '</div>';
        parent.querySelector('#contact-scan-cancel').addEventListener('click', closeAll);
        parent.querySelector('#contact-scan-add').addEventListener('click', function() {
            var btn = parent.querySelector('#contact-scan-add');
            btn.disabled = true;
            btn.textContent = 'Adding...';
            RS.invoke('import_contact_card', { payload: payload }).then(function() {
                showToast('Contact added', 'toast-success', 3000);
                closeAll();
            }).catch(function(err) {
                btn.disabled = false;
                btn.textContent = 'Add';
                showToast(err && err.message ? err.message : 'Could not add contact', 'toast-error', 3000);
            });
        });
    }

    function openContactQrScanner(options) {
        options = options || {};
        var scannerTitle = options.title || 'Scan QR Code';
        var checkingText = options.checkingText || 'Checking QR\u2026';
        var invalidText = options.invalidText || 'That QR isn\'t a contact card, paper message, or lxma:// address.';
        var invalidImageText = options.invalidImageText || invalidText;
        var emptyImageText = options.emptyImageText ||
            'No QR code found in that image.';
        var previewCommand = options.previewCommand || 'api_preview_contact_card';
        var built = buildSheet('contact-scan-sheet');
        var stream = null;
        var stopped = false;
        var liveActive = false;
        var liveStarting = false;
        var detecting = false;
        var firstDetectLogged = false;
        built.sheet.innerHTML =
            '<div class="contact-card-topbar">' +
                '<div class="contact-scan-title">' + escapeHtml(scannerTitle) + '</div>' +
                '<button class="contact-card-close" type="button" aria-label="Close">&times;</button>' +
            '</div>' +
            '<div class="contact-scan-body">' +
                '<div class="contact-scan-camera-wrap">' +
                    '<video class="contact-scan-video" autoplay muted playsinline></video>' +
                    '<div class="contact-scan-fallback-label">' + iconSvg('image') + '<span>Scan Photo</span></div>' +
                    '<div class="contact-scan-frame"></div>' +
                '</div>' +
                '<div class="contact-scan-status">Preparing scanner...</div>' +
                '<div class="contact-scan-picker-actions">' +
                    '<button class="nr-btn contact-scan-live-btn" type="button" id="contact-scan-live">' + iconSvg('scan') + '<span>Live Scan</span></button>' +
                    '<button class="nr-btn nr-btn-ghost contact-scan-file-btn" type="button" id="contact-scan-file">' + iconSvg('image') + '<span>Scan Photo</span></button>' +
                '</div>' +
                '<input class="contact-scan-file-input" type="file" accept="image/*">' +
            '</div>';

        var body = built.sheet.querySelector('.contact-scan-body');
        var status = built.sheet.querySelector('.contact-scan-status');
        var video = built.sheet.querySelector('video');
        var cameraWrap = built.sheet.querySelector('.contact-scan-camera-wrap');
        var fileInput = built.sheet.querySelector('.contact-scan-file-input');
        var fileButton = built.sheet.querySelector('#contact-scan-file');
        var liveButton = built.sheet.querySelector('#contact-scan-live');
        var scanCanvas = document.createElement('canvas');
        var scanCtx = null;
        try {
            scanCtx = scanCanvas.getContext('2d', { willReadFrequently: true });
        } catch (_) {
            scanCtx = scanCanvas.getContext('2d');
        }

        function getScannerEnvironment() {
            if (!window.RatspeakAndroid || typeof window.RatspeakAndroid.getQrScannerEnvironment !== 'function') {
                return null;
            }
            try {
                return JSON.parse(window.RatspeakAndroid.getQrScannerEnvironment() || '{}');
            } catch (err) {
                contactScanDiag('android_env_parse_failed', { message: err && err.message ? err.message : String(err) });
                return null;
            }
        }

        function scanErrorDetail(err) {
            if (!err) return {};
            return {
                name: err.name || '',
                message: err.message || String(err),
            };
        }

        function stopStream() {
            liveActive = false;
            detecting = false;
            if (stream && typeof _rsStopMediaStream === 'function') _rsStopMediaStream(stream);
            else if (stream && stream.getTracks) stream.getTracks().forEach(function(t) { try { t.stop(); } catch (_) {} });
            stream = null;
            if (video) {
                try { video.pause(); } catch (_) {}
                try { video.srcObject = null; } catch (_) {}
            }
        }

        function stop() {
            stopped = true;
            stopStream();
        }

        function closeAll() {
            stop();
            closeSheet(built.overlay, built.sheet);
        }

        // Native Android Back must take the same path as the visible close
        // controls so an active camera stream is stopped before removal.
        built.sheet._ratspeakDismiss = closeAll;

        function setFallbackReady(ready) {
            if (!cameraWrap) return;
            cameraWrap.classList.toggle('fallback-ready', !!ready);
        }

        function setScannerMode(mode) {
            var fallback = mode === 'fallback';
            built.sheet.classList.toggle('contact-scan-prefers-fallback', fallback);
            built.sheet.classList.toggle('contact-scan-prefers-live', !fallback);
            liveButton.classList.toggle('nr-btn-ghost', fallback);
            fileButton.classList.toggle('nr-btn-ghost', !fallback);
        }

        function setLiveButtonEnabled(enabled) {
            liveButton.disabled = !enabled;
        }

        function handleScannedPayload(payload, source, invalidMessage, retry) {
            status.textContent = checkingText;
            contactScanDiag('payload_detected', { source: source });
            var paperUri = String(payload || '').trim();
            if (/^lxm:\/\//i.test(paperUri) && window.RS.paper) {
                return RS.paper.preview(paperUri).then(function(preview) {
                    stopStream();
                    RS.paper.renderResult(body, paperUri, preview, { close: closeAll });
                }).catch(function(err) {
                    contactScanDiag('paper_rejected', scanErrorDetail(err));
                    if (RS.paper.errorCode(err) === 'locked') {
                        closeAll();
                        return;
                    }
                    status.textContent = RS.paper.errorText(err);
                    if (typeof retry === 'function') retry();
                });
            }
            return RS.invoke(previewCommand, { payload: payload }).then(function(preview) {
                stopStream();
                if (typeof options.onPreview === 'function') {
                    options.onPreview(body, payload, preview, closeAll);
                } else {
                    showScannedCardPreview(body, payload, preview, closeAll);
                }
            }).catch(function(err) {
                contactScanDiag('payload_rejected', scanErrorDetail(err));
                var legacyReason = /^lxma:\/\//i.test(paperUri) && err && err.message;
                status.textContent = legacyReason || invalidMessage || invalidText;
                if (typeof retry === 'function') retry();
            });
        }

        function nativeQrDetectorAvailable() {
            if (!('BarcodeDetector' in window)) {
                contactScanDiag('barcode_detector_missing');
                return Promise.resolve(false);
            }
            if (typeof BarcodeDetector.getSupportedFormats !== 'function') return Promise.resolve(true);
            try {
                return BarcodeDetector.getSupportedFormats().then(function(formats) {
                    var supported = Array.isArray(formats) && formats.indexOf('qr_code') !== -1;
                    contactScanDiag('barcode_detector_formats', { qr_code: supported, count: Array.isArray(formats) ? formats.length : 0 });
                    return supported;
                }).catch(function(err) {
                    contactScanDiag('barcode_detector_formats_failed', scanErrorDetail(err));
                    return false;
                });
            } catch (err) {
                contactScanDiag('barcode_detector_formats_threw', scanErrorDetail(err));
                return Promise.resolve(false);
            }
        }

        function decodeImageFile(file) {
            if (!file) return;
            stopStream();
            setFallbackReady(true);
            setScannerMode('fallback');
            setLiveButtonEnabled(true);
            status.textContent = 'Reading QR image...';
            contactScanDiag('file_selected', {
                type: file.type || '',
                size: file.size || 0,
            });
            if (typeof window.jsQR !== 'function') {
                contactScanDiag('file_decoder_missing');
                status.textContent = 'QR image scanning is not available in this build.';
                return;
            }
            var img = new Image();
            var url = URL.createObjectURL(file);
            img.onload = function() {
                try {
                    var naturalWidth = img.naturalWidth || img.width;
                    var naturalHeight = img.naturalHeight || img.height;
                    if (!naturalWidth || !naturalHeight) throw new Error('Image has no readable dimensions.');
                    var scale = Math.min(1, CONTACT_SCAN_MAX_IMAGE_DIMENSION / Math.max(naturalWidth, naturalHeight));
                    var width = Math.max(1, Math.round(naturalWidth * scale));
                    var height = Math.max(1, Math.round(naturalHeight * scale));
                    var canvas = document.createElement('canvas');
                    canvas.width = width;
                    canvas.height = height;
                    var ctx = null;
                    try {
                        ctx = canvas.getContext('2d', { willReadFrequently: true });
                    } catch (_) {
                        ctx = canvas.getContext('2d');
                    }
                    if (!ctx) throw new Error('Could not read image pixels.');
                    ctx.drawImage(img, 0, 0, width, height);
                    var image = ctx.getImageData(0, 0, width, height);
                    var code = window.jsQR(image.data, width, height, { inversionAttempts: 'attemptBoth' });
                    var finish = function(found) {
                        if (stopped) return;
                        if (found && found.data) {
                            contactScanDiag('file_decode_success', { width: width, height: height });
                            handleScannedPayload(found.data, 'file', invalidImageText);
                        } else {
                            contactScanDiag('file_decode_empty', { width: width, height: height });
                            status.textContent = emptyImageText;
                        }
                    };
                    if (code && code.data) {
                        finish(code);
                    } else {
                        // The gap-closing passes take about a second on slow phones; paint first.
                        status.textContent = checkingText;
                        var afterPaint = window.requestAnimationFrame || function(callback) { return setTimeout(callback, 16); };
                        afterPaint(function() {
                            setTimeout(function() {
                                try {
                                    finish(decodeRoundedQr(image.data, width, height));
                                } catch (retryErr) {
                                    contactScanDiag('file_decode_failed', scanErrorDetail(retryErr));
                                    status.textContent = 'Could not read that QR image.';
                                }
                            }, 0);
                        });
                    }
                } catch (err) {
                    contactScanDiag('file_decode_failed', scanErrorDetail(err));
                    status.textContent = err && err.message ? err.message : 'Could not read that QR image.';
                } finally {
                    URL.revokeObjectURL(url);
                }
            };
            img.onerror = function() {
                contactScanDiag('file_image_load_failed');
                URL.revokeObjectURL(url);
                status.textContent = 'Could not open that image.';
            };
            img.src = url;
        }

        function startLiveScanner(reason) {
            if (stopped || liveActive || liveStarting) return;
            liveStarting = true;
            firstDetectLogged = false;
            setFallbackReady(false);
            setScannerMode('live');
            setLiveButtonEnabled(true);
            status.textContent = 'Opening camera...';
            contactScanDiag('live_start_requested', { reason: reason || 'manual' });

            nativeQrDetectorAvailable().then(function(available) {
                if (!available) {
                    liveStarting = false;
                    setFallbackReady(true);
                    setScannerMode('fallback');
                    setLiveButtonEnabled(false);
                    status.textContent = 'Live QR scanning is not available in this WebView. Use Scan Photo.';
                    return;
                }
                if (!navigator.mediaDevices || typeof navigator.mediaDevices.getUserMedia !== 'function') {
                    liveStarting = false;
                    setFallbackReady(true);
                    setScannerMode('fallback');
                    setLiveButtonEnabled(false);
                    status.textContent = 'Camera access is not available in this WebView. Use Scan Photo.';
                    contactScanDiag('get_user_media_missing');
                    return;
                }
                var permission = (window.RS && RS.mediaPermissions && RS.mediaPermissions.ensure)
                    ? RS.mediaPermissions.ensure({ camera: true })
                    : Promise.resolve(true);

                contactScanDiag('permission_request');
                permission.then(function(granted) {
                    contactScanDiag('permission_result', { granted: !!granted });
                    if (!granted) {
                        liveStarting = false;
                        setFallbackReady(true);
                        setScannerMode('fallback');
                        setLiveButtonEnabled(true);
                        status.textContent = 'Camera permission is required for live scan. You can still use Scan Photo.';
                        return null;
                    }
                    var constraints = {
                        video: { facingMode: { ideal: 'environment' } },
                        audio: false,
                    };
                    contactScanDiag('get_user_media_start', { facingMode: 'environment' });
                    return navigator.mediaDevices.getUserMedia(constraints);
                }).then(function(s) {
                    if (!s || stopped) return;
                    stream = s;
                    liveStarting = false;
                    liveActive = true;
                    video.srcObject = stream;
                    status.textContent = 'Point the camera at a Ratspeak QR.';
                    var tracks = stream.getVideoTracks ? stream.getVideoTracks() : [];
                    contactScanDiag('get_user_media_ready', { video_tracks: tracks.length });
                    var detector = null;
                    try {
                        detector = new BarcodeDetector({ formats: ['qr_code'] });
                        contactScanDiag('detector_created');
                    } catch (err) {
                        liveStarting = false;
                        stopStream();
                        setFallbackReady(true);
                        setScannerMode('fallback');
                        setLiveButtonEnabled(false);
                        status.textContent = 'Live QR scanning is not available in this WebView. Use Scan Photo.';
                        contactScanDiag('detector_create_failed', scanErrorDetail(err));
                        return;
                    }
                    var scanStarted = false;

                    function scheduleScan() {
                        if (stopped || !liveActive) return;
                        setTimeout(function() {
                            if (!stopped && liveActive) requestAnimationFrame(scan);
                        }, 90);
                    }

                    function detectFrame() {
                        if (!firstDetectLogged) {
                            firstDetectLogged = true;
                            contactScanDiag('first_detect_start');
                        }
                        if (!scanCtx || video.readyState < 2 || !video.videoWidth || !video.videoHeight) {
                            return detector.detect(video);
                        }
                        var vw = video.videoWidth;
                        var vh = video.videoHeight;
                        var side = Math.min(vw, vh);
                        var sx = (vw - side) / 2;
                        var sy = (vh - side) / 2;
                        var target = Math.round(Math.max(480, Math.min(900, side)));
                        if (scanCanvas.width !== target) scanCanvas.width = target;
                        if (scanCanvas.height !== target) scanCanvas.height = target;
                        scanCtx.drawImage(video, sx, sy, side, side, 0, 0, target, target);
                        return detector.detect(scanCanvas).then(function(codes) {
                            if (codes && codes.length) return codes;
                            return detector.detect(video).catch(function() { return codes || []; });
                        }).catch(function() {
                            return detector.detect(video).catch(function() { return []; });
                        });
                    }

                    function scan() {
                        if (stopped || !liveActive || detecting) return;
                        if (video.readyState < 2) {
                            scheduleScan();
                            return;
                        }
                        detecting = true;
                        var detection = null;
                        try {
                            detection = detectFrame();
                        } catch (err) {
                            contactScanDiag('detect_frame_threw', scanErrorDetail(err));
                            detecting = false;
                            scheduleScan();
                            return;
                        }
                        detection.then(function(codes) {
                            detecting = false;
                            if (stopped || !liveActive) return;
                            if (firstDetectLogged === true) {
                                contactScanDiag('first_detect_resolved', { count: codes && codes.length ? codes.length : 0 });
                                firstDetectLogged = 'done';
                            }
                            if (codes && codes.length && codes[0].rawValue) {
                                handleScannedPayload(codes[0].rawValue, 'live', invalidText, function() {
                                    setTimeout(function() {
                                        if (!stopped && liveActive) {
                                            status.textContent = 'Point the camera at a Ratspeak QR.';
                                            scheduleScan();
                                        }
                                    }, 1200);
                                });
                                return;
                            }
                            scheduleScan();
                        }).catch(function(err) {
                            detecting = false;
                            if (firstDetectLogged === true) {
                                contactScanDiag('first_detect_failed', scanErrorDetail(err));
                                firstDetectLogged = 'done';
                            }
                            scheduleScan();
                        });
                    }

                    function beginScanning() {
                        if (scanStarted || stopped || !liveActive) return;
                        scanStarted = true;
                        video.play().catch(function() {});
                        scheduleScan();
                    }
                    video.addEventListener('loadedmetadata', beginScanning, { once: true });
                    video.addEventListener('canplay', beginScanning, { once: true });
                    video.play().then(beginScanning).catch(function(err) {
                        contactScanDiag('video_play_failed', scanErrorDetail(err));
                    });
                }).catch(function(err) {
                    liveStarting = false;
                    stopStream();
                    setFallbackReady(true);
                    setScannerMode('fallback');
                    setLiveButtonEnabled(true);
                    contactScanDiag('get_user_media_failed', scanErrorDetail(err));
                    status.textContent = err && err.message ? err.message : 'Could not open camera. Use Scan Photo.';
                });
            });
        }

        built.overlay.addEventListener('click', function(e) {
            if (e.target === built.overlay) closeAll();
        });
        built.sheet.querySelector('.contact-card-close').addEventListener('click', closeAll);

        function openImageInput(input, message, diagStep) {
            contactScanDiag(diagStep);
            stopStream();
            setFallbackReady(true);
            setScannerMode('fallback');
            setLiveButtonEnabled(true);
            status.textContent = message;
            input.value = '';
            input.click();
        }

        fileButton.addEventListener('click', function() {
            openImageInput(fileInput, 'Choose a QR image.', 'file_picker_open');
        });
        fileInput.addEventListener('change', function() {
            decodeImageFile(fileInput.files && fileInput.files[0]);
        });
        liveButton.addEventListener('click', function() {
            startLiveScanner('manual');
        });

        var env = getScannerEnvironment();
        contactScanDiag('sheet_open', env || {});
        if (env && env.platform === 'android' && env.prefer_live_scanner === false) {
            setFallbackReady(true);
            setScannerMode('fallback');
            setLiveButtonEnabled(true);
            status.textContent = 'Use Scan Photo on this WebView, or try Live Scan.';
        } else {
            setScannerMode('live');
            setLiveButtonEnabled(true);
            requestAnimationFrame(function() { startLiveScanner('auto'); });
        }
    }

    function addContactByAddress() {
        rsPromptContact({ title: 'Add Contact' }).then(function(result) {
            if (!result) return;
            RS.invokeOrToast('add_contact', { args: { hash: result.hash, display_name: result.display_name } }, 'Could not add contact');
        });
    }

    function isMobileContactFlow() {
        return isCompactLayout();
    }

    function closeContactAddDial() {
        if (!activeContactAddDial) return;
        var dial = activeContactAddDial;
        activeContactAddDial = null;

        if (dial.trigger) {
            dial.trigger.classList.remove('dial-open');
            dial.trigger.setAttribute('aria-expanded', 'false');
        }
        dial.actions.classList.remove('open');
        dial.scrim.classList.remove('active');
        document.removeEventListener('keydown', dial.onKey, true);

        setTimeout(function() {
            if (dial.scrim.parentNode) dial.scrim.remove();
            if (dial.actions.parentNode) dial.actions.remove();
        }, 130);
    }

    function showContactAddDial(trigger, items) {
        if (!trigger || !items || !items.length) return false;
        if (activeContactAddDial && activeContactAddDial.trigger === trigger) {
            closeContactAddDial();
            return true;
        }
        closeContactAddDial();

        var scrim = document.createElement('div');
        scrim.className = 'fab-dial-scrim contact-add-dial-scrim';
        scrim.addEventListener('mousedown', function(e) { e.preventDefault(); });
        scrim.addEventListener('touchstart', function(e) { e.preventDefault(); }, { passive: false });
        scrim.addEventListener('touchend', function(e) { e.preventDefault(); closeContactAddDial(); });
        scrim.addEventListener('click', closeContactAddDial);

        var actions = document.createElement('div');
        actions.className = 'fab-dial-actions contact-add-dial-actions';
        actions.setAttribute('role', 'menu');

        items.forEach(function(item) {
            var row = document.createElement('div');
            row.className = 'fab-dial-item';
            row.setAttribute('role', 'menuitem');

            var label = document.createElement('span');
            label.className = 'fab-dial-label';
            label.textContent = item.label;

            var btn = document.createElement('button');
            btn.type = 'button';
            btn.className = 'fab-dial-btn';
            btn.setAttribute('aria-label', item.label);
            btn.innerHTML = item.icon || '';

            var activated = false;
            function activate(e) {
                if (activated) return;
                activated = true;
                if (e) {
                    e.preventDefault();
                    e.stopPropagation();
                }
                if (typeof haptic === 'function') haptic('selection');
                closeContactAddDial();
                if (typeof item.onSelect === 'function') setTimeout(item.onSelect, 0);
            }

            row.addEventListener('mousedown', function(e) { e.preventDefault(); });
            row.addEventListener('touchstart', function(e) { e.preventDefault(); }, { passive: false });
            row.addEventListener('touchend', function(e) {
                var t = (e.changedTouches && e.changedTouches[0]) || null;
                if (t) {
                    var hit = document.elementFromPoint(t.clientX, t.clientY);
                    if (hit !== row && !row.contains(hit)) return;
                }
                activate(e);
            });
            row.addEventListener('click', activate);

            row.appendChild(label);
            row.appendChild(btn);
            actions.appendChild(row);
        });

        function onKey(e) {
            if (e.key === 'Escape') {
                e.stopPropagation();
                closeContactAddDial();
            }
        }

        document.body.appendChild(scrim);
        document.body.appendChild(actions);
        activeContactAddDial = {
            trigger: trigger,
            scrim: scrim,
            actions: actions,
            onKey: onKey,
        };
        document.addEventListener('keydown', onKey, true);

        trigger.classList.add('dial-open');
        trigger.setAttribute('aria-expanded', 'true');
        requestAnimationFrame(function() {
            scrim.classList.add('active');
            actions.classList.add('open');
        });
        return true;
    }

    function openContactAddOptions(trigger) {
        var items = [
            {
                label: 'Address',
                icon: iconSvg('address'),
                onSelect: addContactByAddress,
            },
            {
                label: 'QR',
                icon: iconSvg('qr'),
                onSelect: openContactQrScanner,
            },
        ];
        if (isMobileContactFlow() && showContactAddDial(trigger, items)) {
            return;
        }
        if (typeof actionPopover === 'function') {
            actionPopover(trigger, items, { mobileSheet: false });
        } else {
            addContactByAddress();
        }
    }

    window.RSContactCard = {
        renderQrCanvas: renderQrCanvas,
        shareQrCanvas: shareQrCanvas,
        openIdentityShareScreen: showIdentityShareScreen,
        openContactQrScanner: openContactQrScanner,
        openContactAddOptions: openContactAddOptions,
    };
    window.RS.qr = {
        openPaperShare: showPaperShareScreen,
        renderCanvas: renderQrCanvas,
        renderLegacyCanvas: renderLegacyQrCanvas,
        renderPaperCanvas: renderPaperQrCanvas,
        paperPlan: paperQrPlan,
        encode: QrEncode,
        decodeImageData: function(rgba, width, height) {
            var code = window.jsQR(rgba, width, height, { inversionAttempts: 'attemptBoth' });
            return code && code.data ? code : decodeRoundedQr(rgba, width, height);
        },
        byteCapacity: qrByteCapacity,
        shareCanvas: shareQrCanvas,
        openScanner: openContactQrScanner,
    };
    window.closeContactAddDial = closeContactAddDial;
    window.openIdentityShareScreen = showIdentityShareScreen;
    window.openContactAddOptions = openContactAddOptions;
    window.openContactQrScanner = openContactQrScanner;
})();
