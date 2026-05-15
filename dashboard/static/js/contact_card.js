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

    function drawRatspeakLogo(ctx, cx, cy, size) {
        ctx.save();
        roundRect(ctx, cx - size / 2, cy - size / 2, size, size, size * 0.16);
        ctx.fillStyle = '#ffffff';
        ctx.fill();
        ctx.lineWidth = Math.max(2, size * 0.025);
        ctx.strokeStyle = 'rgba(210, 105, 59, 0.28)';
        ctx.stroke();

        var w = size * 0.56;
        var h = size * 0.38;
        var x = cx - w / 2;
        var y = cy - h / 2;
        ctx.strokeStyle = '#D2693B';
        ctx.lineWidth = Math.max(3, size * 0.045);
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
        ctx.beginPath();
        ctx.arc(x + w * 0.42, y + h * 0.38, Math.max(2, size * 0.025), 0, Math.PI * 2);
        ctx.arc(x + w * 0.63, y + h * 0.46, Math.max(2, size * 0.025), 0, Math.PI * 2);
        ctx.fillStyle = '#D2693B';
        ctx.fill();
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
        ctx.fillStyle = '#ffffff';
        ctx.fillRect(0, 0, pixels, pixels);
        var cell = pixels / modules;
        ctx.fillStyle = '#050505';
        for (var y = 0; y < qr.size; y++) {
            for (var x = 0; x < qr.size; x++) {
                if (!qr.modules[y][x]) continue;
                var px = (x + quiet) * cell;
                var py = (y + quiet) * cell;
                roundRect(ctx, px + cell * 0.08, py + cell * 0.08, cell * 0.84, cell * 0.84, cell * 0.24);
                ctx.fill();
            }
        }
        drawRatspeakLogo(ctx, pixels / 2, pixels / 2, pixels * 0.17);
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
                    '<div class="contact-share-address mono">' + escapeHtml(shortValue(card.lxmf_hash, 12, 8)) + '</div>' +
                '</div>' +
                '<div class="contact-share-actions">' +
                    '<button class="nr-btn contact-share-action" id="contact-copy-address">' + iconSvg('copy') + '<span>Copy Address</span></button>' +
                    '<button class="nr-btn contact-share-action" id="contact-share-qr">' + iconSvg('qr') + '<span>Share QR</span></button>' +
                    '<button class="nr-btn contact-share-action" id="contact-share-card">' + iconSvg('share') + '<span>Share Card</span></button>' +
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
            built.sheet.querySelector('#contact-share-card').addEventListener('click', function() {
                if (navigator.share) {
                    navigator.share({
                        title: name + ' on Ratspeak',
                        text: card.payload || card.lxmf_hash,
                    }).catch(function() {});
                } else {
                    copyText(card.payload || card.lxmf_hash, 'Contact Card').then(function(ok) {
                        if (!ok) showToast('Could not share contact card', 'toast-orange', 2500);
                    });
                }
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

            function scan() {
                if (stopped || detecting) return;
                detecting = true;
                detector.detect(video).then(function(codes) {
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
                                if (!stopped) status.textContent = 'Point the camera at a Ratspeak QR.';
                            }, 1400);
                            requestAnimationFrame(scan);
                        });
                        return;
                    }
                    requestAnimationFrame(scan);
                }).catch(function() {
                    detecting = false;
                    if (!stopped) requestAnimationFrame(scan);
                });
            }
            video.addEventListener('loadedmetadata', function() {
                video.play().catch(function() {});
                requestAnimationFrame(scan);
            }, { once: true });
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
