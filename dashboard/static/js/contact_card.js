(function() {
    var CONTACT_QR_FILE = 'ratspeak-contact-card.png';
    var activeContactAddDial = null;

    function iconSvg(name) {
        if (name === 'qr') {
            return '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="3" width="7" height="7"/><rect x="14" y="3" width="7" height="7"/><rect x="3" y="14" width="7" height="7"/><path d="M14 14h3v3h-3z"/><path d="M19 14h2"/><path d="M14 21h7v-2"/><path d="M19 17h2"/></svg>';
        }
        if (name === 'copy') {
            return '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2"/><rect x="2" y="2" width="13" height="13" rx="2"/></svg>';
        }
        if (name === 'share') {
            return '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="18" cy="5" r="3"/><circle cx="6" cy="12" r="3"/><circle cx="18" cy="19" r="3"/><path d="M8.59 13.51l6.82 3.98"/><path d="M15.41 6.51 8.59 10.49"/></svg>';
        }
        if (name === 'address') {
            return '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 21s7-4.35 7-11a7 7 0 1 0-14 0c0 6.65 7 11 7 11Z"/><circle cx="12" cy="10" r="2.5"/></svg>';
        }
        if (name === 'check') {
            return '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6 9 17l-5-5"/></svg>';
        }
        return '';
    }

    function copyText(value, label) {
        if (!value) return Promise.resolve(false);
        if (navigator.clipboard && navigator.clipboard.writeText) {
            return navigator.clipboard.writeText(value).then(function() {
                if (typeof showCopyConfirmationToast === 'function') showCopyConfirmationToast(label || 'Value');
                return true;
            }).catch(function() { return false; });
        }
        return Promise.resolve(false);
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

    function QrV9L(text) {
        var VERSION = 9;
        var SIZE = 53;
        var DATA_CODEWORDS = 232;
        var ECC_CODEWORDS_PER_BLOCK = 30;
        var NUM_BLOCKS = 2;
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
            [6, 26, 46].forEach(function(x) {
                [6, 26, 46].forEach(function(y) {
                    if ((x === 6 && y === 6) || (x === 46 && y === 6) || (x === 6 && y === 46)) return;
                    drawAlignment(x, y);
                });
            });
            setFunction(8, 4 * VERSION + 9, true);
            drawVersionBits();
            drawFormatBits(0);
        }

        function drawFormatBits(mask) {
            var data = (1 << 3) | mask; // Error correction L, mask 0.
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
            if (bytes.length > 230) {
                throw new Error('Contact card is too large for the QR layout');
            }
            var bits = [];
            appendBits(bits, 0x4, 4);
            appendBits(bits, bytes.length, 8);
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
            for (var block = 0; block < NUM_BLOCKS; block++) {
                var part = data.slice(offset, offset + 116);
                offset += 116;
                blocks.push({ data: part, ecc: reedSolomonRemainder(part, divisor) });
            }

            var out = [];
            for (var x = 0; x < 116; x++) {
                for (var b = 0; b < NUM_BLOCKS; b++) out.push(blocks[b].data[x]);
            }
            for (var e = 0; e < ECC_CODEWORDS_PER_BLOCK; e++) {
                for (var c = 0; c < NUM_BLOCKS; c++) out.push(blocks[c].ecc[e]);
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
                        var bit = bitIndex < bits.length ? bits[bitIndex++] : 0;
                        var mask = ((x + y) % 2) === 0;
                        modules[y][x] = !!(bit ^ (mask ? 1 : 0));
                    }
                }
                upward = !upward;
            }
            drawFormatBits(0);
        }

        drawFunctionPatterns();
        drawCodewords(encodeCodewords(text));
        return { size: SIZE, modules: modules };
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
            ctx.fillStyle = color || '#D2693B';
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
        ctx.strokeStyle = '#D2693B';
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
        ctx.shadowColor = 'rgba(42, 37, 34, 0.12)';
        ctx.shadowBlur = size * 0.14;
        ctx.shadowOffsetY = size * 0.035;
        roundRect(ctx, cx - size / 2, cy - size / 2, size, size, size * 0.18);
        ctx.fillStyle = surface || '#fffaf3';
        ctx.fill();
        ctx.shadowColor = 'transparent';
        ctx.lineWidth = Math.max(2, size * 0.025);
        ctx.strokeStyle = 'rgba(210, 105, 59, 0.22)';
        ctx.stroke();
        if (!drawOfficialRatspeakMark(ctx, cx, cy, size * 0.72, '#D2693B')) {
            drawFallbackRatspeakMark(ctx, cx, cy, size * 0.78);
        }
        ctx.restore();
    }

    function renderQrCanvas(canvas, payload) {
        var qr = QrV9L(payload);
        var quiet = 4;
        var modules = qr.size + quiet * 2;
        var pixels = 900;
        canvas.width = pixels;
        canvas.height = pixels;
        var ctx = canvas.getContext('2d');
        var qrSurface = '#fffaf3';
        ctx.fillStyle = qrSurface;
        ctx.fillRect(0, 0, pixels, pixels);
        var cell = pixels / modules;
        var logoSize = pixels * 0.12;
        var logoClearSize = logoSize * 1.04;
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
                if (moduleFallsBehindLogo(px, py)) continue;
                roundRect(ctx, px + cell * 0.08, py + cell * 0.08, cell * 0.84, cell * 0.84, cell * 0.24);
                ctx.fill();
            }
        }
        drawRatspeakLogo(ctx, pixels / 2, pixels / 2, logoSize, qrSurface);
        return canvas;
    }

    function canvasBlob(canvas) {
        return new Promise(function(resolve, reject) {
            canvas.toBlob(function(blob) {
                if (blob) resolve(blob);
                else reject(new Error('Could not render QR image'));
            }, 'image/png');
        });
    }

    function saveQrBlob(blob, fileName) {
        if (window.File && navigator.canShare && navigator.share) {
            try {
                var file = new File([blob], fileName, { type: 'image/png' });
                if (navigator.canShare({ files: [file] })) {
                    return navigator.share({ files: [file], title: 'Ratspeak Contact Card' })
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

    function closeSheet(overlay, sheet, onClose) {
        sheet.classList.remove('open');
        overlay.classList.remove('active');
        setTimeout(function() {
            if (overlay.parentNode) overlay.remove();
            if (sheet.parentNode) sheet.remove();
            if (typeof onClose === 'function') onClose();
        }, 180);
    }

    function buildSheet(className) {
        var overlay = document.createElement('div');
        overlay.className = 'bottom-sheet-overlay contact-card-overlay';
        var sheet = document.createElement('div');
        sheet.className = className;
        document.body.appendChild(overlay);
        document.body.appendChild(sheet);
        sheet.offsetHeight;
        overlay.classList.add('active');
        sheet.classList.add('open');
        return { overlay: overlay, sheet: sheet };
    }

    function showIdentityShareScreen(identityHash) {
        RS.invoke('api_contact_card', { hashHex: identityHash || null }).then(function(card) {
            var name = card.display_name || 'Ratspeak Contact';
            var fileBase = safeFileBase(name);
            var built = buildSheet('contact-share-sheet');
            built.sheet.innerHTML =
                '<div class="contact-card-topbar">' +
                    '<button class="contact-card-close" type="button" aria-label="Close">&times;</button>' +
                '</div>' +
                '<div class="contact-share-identity">' +
                    '<div class="contact-share-avatar">' + (typeof identityAvatar === 'function' ? identityAvatar(card.lxmf_hash, 56) : '') + '</div>' +
                    '<div class="contact-share-name">' + escapeHtml(name) + '</div>' +
                    '<div class="contact-share-subtitle">Ratspeak Contact Card</div>' +
                '</div>' +
                '<div class="contact-share-qr-shell">' +
                    '<canvas class="contact-share-qr" aria-label="Ratspeak contact card QR"></canvas>' +
                '</div>' +
                '<div class="contact-share-address-block">' +
                    '<div class="contact-share-address-label">LXMF Address</div>' +
                    '<div class="contact-share-address mono">' + escapeHtml(card.lxmf_hash || '') + '</div>' +
                '</div>' +
                '<div class="contact-share-actions">' +
                    '<button class="nr-btn contact-share-action" id="contact-copy-address">' + iconSvg('copy') + '<span>Copy</span></button>' +
                    '<button class="nr-btn contact-share-action" id="contact-share-qr">' + iconSvg('qr') + '<span>Share QR</span></button>' +
                '</div>';

            var canvas = built.sheet.querySelector('canvas');
            try {
                renderQrCanvas(canvas, card.payload || '');
            } catch (err) {
                showToast(err && err.message ? err.message : 'Could not render QR', 'toast-red', 3000);
            }

            built.overlay.addEventListener('click', function(e) {
                if (e.target === built.overlay) closeSheet(built.overlay, built.sheet);
            });
            built.sheet.querySelector('.contact-card-close').addEventListener('click', function() {
                closeSheet(built.overlay, built.sheet);
            });
            built.sheet.querySelector('#contact-copy-address').addEventListener('click', function() {
                copyText(card.lxmf_hash, 'Address');
            });
            built.sheet.querySelector('#contact-share-qr').addEventListener('click', function() {
                canvasBlob(canvas).then(function(blob) {
                    return saveQrBlob(blob, fileBase + '-' + CONTACT_QR_FILE);
                }).then(function(method) {
                    if (method === 'share') showToast('QR handed to destination', 'toast-green', 2500);
                    else showToast('QR image saved', 'toast-green', 2500);
                }).catch(function(err) {
                    showToast(err && err.message ? err.message : 'Could not share QR', 'toast-red', 3000);
                });
            });
        }).catch(function(err) {
            showToast(err && err.message ? err.message : 'Could not build contact card', 'toast-red', 3000);
        });
    }

    function showScannedCardPreview(parent, payload, card, closeAll) {
        var name = card.display_name || 'Unnamed Contact';
        parent.innerHTML =
            '<div class="contact-scan-success">' +
                '<div class="contact-scan-check">' + iconSvg('check') + '</div>' +
                '<div class="contact-scan-avatar">' + (typeof identityAvatar === 'function' ? identityAvatar(card.lxmf_hash, 72) : '') + '</div>' +
                '<div class="contact-scan-name">' + escapeHtml(name) + '</div>' +
                '<div class="contact-scan-subtitle">Contact card verified</div>' +
                '<div class="contact-card-detail-list">' +
                    '<div class="contact-card-detail-row"><span>LXMF Address</span><code>' + escapeHtml(shortValue(card.lxmf_hash, 12, 8)) + '</code></div>' +
                    '<div class="contact-card-detail-row"><span>Identity Hash</span><code>' + escapeHtml(shortValue(card.identity_hash, 12, 8)) + '</code></div>' +
                    '<div class="contact-card-detail-row"><span>Public Identity Key</span><code>' + escapeHtml(shortValue(card.public_key, 12, 10)) + '</code></div>' +
                '</div>' +
                '<div class="contact-scan-actions">' +
                    '<button class="nr-btn nr-btn-ghost" id="contact-scan-cancel">Cancel</button>' +
                    '<button class="nr-btn" id="contact-scan-add">Add</button>' +
                '</div>' +
            '</div>';
        parent.querySelector('#contact-scan-cancel').addEventListener('click', closeAll);
        parent.querySelector('#contact-scan-add').addEventListener('click', function() {
            var btn = parent.querySelector('#contact-scan-add');
            btn.disabled = true;
            btn.textContent = 'Adding...';
            RS.invoke('import_contact_card', { payload: payload }).then(function() {
                showToast('Contact added with identity key', 'toast-green', 3000);
                closeAll();
            }).catch(function(err) {
                btn.disabled = false;
                btn.textContent = 'Add';
                showToast(err && err.message ? err.message : 'Could not add contact', 'toast-red', 3000);
            });
        });
    }

    function openContactQrScanner() {
        var built = buildSheet('contact-scan-sheet');
        var stream = null;
        var stopped = false;
        var detecting = false;
        built.sheet.innerHTML =
            '<div class="contact-card-topbar">' +
                '<div class="contact-scan-title">Scan Contact QR</div>' +
                '<button class="contact-card-close" type="button" aria-label="Close">&times;</button>' +
            '</div>' +
            '<div class="contact-scan-body">' +
                '<div class="contact-scan-camera-wrap">' +
                    '<video class="contact-scan-video" autoplay muted playsinline></video>' +
                    '<div class="contact-scan-frame"></div>' +
                '</div>' +
                '<div class="contact-scan-status">Opening camera...</div>' +
            '</div>';

        var body = built.sheet.querySelector('.contact-scan-body');
        var status = built.sheet.querySelector('.contact-scan-status');
        var video = built.sheet.querySelector('video');
        var scanCanvas = document.createElement('canvas');
        var scanCtx = null;
        try {
            scanCtx = scanCanvas.getContext('2d', { willReadFrequently: true });
        } catch (_) {
            scanCtx = scanCanvas.getContext('2d');
        }

        function stop() {
            stopped = true;
            if (stream && typeof _rsStopMediaStream === 'function') _rsStopMediaStream(stream);
            else if (stream && stream.getTracks) stream.getTracks().forEach(function(t) { try { t.stop(); } catch (_) {} });
            stream = null;
        }
        function closeAll() {
            stop();
            closeSheet(built.overlay, built.sheet);
        }
        built.overlay.addEventListener('click', function(e) {
            if (e.target === built.overlay) closeAll();
        });
        built.sheet.querySelector('.contact-card-close').addEventListener('click', closeAll);

        if (!('BarcodeDetector' in window)) {
            status.textContent = 'QR scanning is not available in this WebView.';
            return;
        }

        var permission = (window.RS && RS.mediaPermissions && RS.mediaPermissions.ensure)
            ? RS.mediaPermissions.ensure({ camera: true })
            : Promise.resolve(true);

        permission.then(function(granted) {
            if (!granted) {
                status.textContent = 'Camera permission is required to scan contact cards.';
                return;
            }
            return navigator.mediaDevices.getUserMedia({
                video: { facingMode: { ideal: 'environment' } },
                audio: false,
            });
        }).then(function(s) {
            if (!s) return;
            stream = s;
            video.srcObject = stream;
            status.textContent = 'Point the camera at a Ratspeak QR.';
            var detector = new BarcodeDetector({ formats: ['qr_code'] });
            var scanStarted = false;

            function scheduleScan() {
                if (stopped) return;
                setTimeout(function() {
                    if (!stopped) requestAnimationFrame(scan);
                }, 90);
            }

            function detectFrame() {
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
                if (stopped || detecting) return;
                if (video.readyState < 2) {
                    scheduleScan();
                    return;
                }
                detecting = true;
                var detection = null;
                try {
                    detection = detectFrame();
                } catch (_) {
                    detecting = false;
                    scheduleScan();
                    return;
                }
                detection.then(function(codes) {
                    detecting = false;
                    if (stopped) return;
                    if (codes && codes.length && codes[0].rawValue) {
                        var payload = codes[0].rawValue;
                        status.textContent = 'Checking contact card...';
                        RS.invoke('api_preview_contact_card', { payload: payload }).then(function(card) {
                            stop();
                            showScannedCardPreview(body, payload, card, closeAll);
                        }).catch(function() {
                            status.textContent = 'That QR is not a valid Ratspeak contact card.';
                            setTimeout(function() {
                                if (!stopped) {
                                    status.textContent = 'Point the camera at a Ratspeak QR.';
                                    scheduleScan();
                                }
                            }, 1200);
                        });
                        return;
                    }
                    scheduleScan();
                }).catch(function() {
                    detecting = false;
                    scheduleScan();
                });
            }

            function beginScanning() {
                if (scanStarted) return;
                scanStarted = true;
                video.play().catch(function() {});
                scheduleScan();
            }
            video.addEventListener('loadedmetadata', beginScanning, { once: true });
            video.addEventListener('canplay', beginScanning, { once: true });
            video.play().then(beginScanning).catch(function() {});
        }).catch(function(err) {
            status.textContent = err && err.message ? err.message : 'Could not open camera.';
        });
    }

    function addContactByAddress() {
        rsPromptContact({ title: 'Add Contact' }).then(function(result) {
            if (!result) return;
            RS.invoke('add_contact', { args: { hash: result.hash, display_name: result.display_name } }).catch(function() {});
            showToast('Adding contact...', 'toast-orange', 2000);
        });
    }

    function isMobileContactFlow() {
        if (typeof isMobile === 'function') return isMobile();
        return window.matchMedia && window.matchMedia('(max-width: 768px)').matches;
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
        openIdentityShareScreen: showIdentityShareScreen,
        openContactQrScanner: openContactQrScanner,
        openContactAddOptions: openContactAddOptions,
    };
    window.closeContactAddDial = closeContactAddDial;
    window.openIdentityShareScreen = showIdentityShareScreen;
    window.openContactAddOptions = openContactAddOptions;
    window.openContactQrScanner = openContactQrScanner;
})();
