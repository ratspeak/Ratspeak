function isCompactLayout() {
    var breakpoint = window.RS && window.RS.config ? window.RS.config.MOBILE_BREAKPOINT : 768;
    return window.innerWidth <= breakpoint;
}

function isTouchDevice() {
    return window.__RATSPEAK_MOBILE__ === true || navigator.maxTouchPoints > 0;
}

// Legacy interaction predicate. Layout decisions should use
// isCompactLayout(); gesture and soft-keyboard decisions use isMobile().
function isMobile() {
    var breakpoint = window.RS && window.RS.config ? window.RS.config.MOBILE_TOUCH_BREAKPOINT : 1024;
    return isTouchDevice() && window.innerWidth <= breakpoint;
}

// Tauri injects __RATSPEAK_MOBILE__ / __RATSPEAK_DESKTOP__ globals.
// userAgent regex covers plain-browser access (ratspeak.org dev).
function isIOS() {
    if (window.__RATSPEAK_DESKTOP__) return false;
    if (/iPhone|iPad|iPod/.test(navigator.userAgent)) return true;
    // iPadOS 13+ reports as desktop Safari — detect via touch + Mac platform.
    return navigator.platform === 'MacIntel' && navigator.maxTouchPoints > 1;
}

function isAndroid() {
    if (window.__RATSPEAK_DESKTOP__) return false;
    return /Android/.test(navigator.userAgent);
}

function isTauriMobile() { return window.__RATSPEAK_MOBILE__ === true; }
function isTauriDesktop() { return window.__RATSPEAK_DESKTOP__ === true; }
function supportsHardwareIdentities() {
    if (isTauriMobile()) return false;
    if (isTauriDesktop()) return true;
    return !isIOS() && !isAndroid();
}
function hasAndroidBridge() {
    return isTauriMobile() && typeof window.RatspeakAndroid !== 'undefined';
}

// Global gesture handlers early-return on this so swipes can't translate
// non-setup views into view during first-run setup.
function _isSetupActive() {
    var b = document.body;
    return b && (b.classList.contains('checking-setup') || b.classList.contains('setup-active'));
}

window.RS = window.RS || {};

window.RS.diag = function(level) {
    if (window.__RATSPEAK_DIAGNOSTICS__ !== true) return;
    var c = window.console;
    if (!c) return;
    var fn = c[level] || c.log;
    if (typeof fn !== 'function') return;
    var args = Array.prototype.slice.call(arguments, 1);
    try { fn.apply(c, args); } catch (_) {}
};

// Shared relative-time formatter (events, health, games). Seconds in;
// falsy/NaN timestamps render '' (missing data, e.g. BLE connected_at).
window.RS.relativeTimeFrom = function(nowSec, thenSec) {
    if (!thenSec) return '';
    var diff = Math.floor(nowSec - thenSec);
    if (diff < 5) return 'just now';
    if (diff < 60) return diff + 's ago';
    if (diff < 3600) return Math.floor(diff / 60) + 'm ago';
    if (diff < 86400) return Math.floor(diff / 3600) + 'h ago';
    return Math.floor(diff / 86400) + 'd ago';
};

window.RS.relativeTime = function(epochSeconds) {
    return window.RS.relativeTimeFrom(Date.now() / 1000, epochSeconds);
};

// Tauri IPC bridge. Resolves with the command return value; rejects with
// an Error whose `.code` is the AppError code (snake_case).
function _rsInvokeAvailable() {
    return typeof window.__TAURI_INTERNALS__ !== 'undefined'
        && typeof window.__TAURI_INTERNALS__.invoke === 'function';
}

function _rsWaitForInvoke() {
    if (_rsInvokeAvailable()) return Promise.resolve();
    return new Promise(function(resolve, reject) {
        var attempts = 0;
        var iv = setInterval(function() {
            attempts++;
            if (_rsInvokeAvailable()) {
                clearInterval(iv);
                resolve();
            } else if (attempts >= 40) {
                clearInterval(iv);
                var err = new Error('Tauri IPC not available in this context');
                err.code = 'ipc_unavailable';
                reject(err);
            }
        }, 50);
    });
}

window.RS.invoke = function(name, args) {
    return _rsWaitForInvoke().then(function() {
        return window.__TAURI_INTERNALS__.invoke(name, args || {});
    }).catch(function(raw) {
        // Preserve `.code` for downstream `err.code === 'not_found'` checks.
        var code = (raw && raw.code) || 'error';
        var message = (raw && raw.message) || (typeof raw === 'string' ? raw : 'Request failed');
        var err = new Error(message);
        err.code = code;
        throw err;
    });
};

// Copy text to the clipboard, resolving true on success. The synchronous
// execCommand path runs first because navigator.clipboard.writeText silently
// fails in macOS WKWebView; execCommand works in every webview as long as it
// runs inside the user gesture.
window.RS.copyText = function(value) {
    var text = String(value == null ? '' : value);
    var ok = false;
    try {
        var ta = document.createElement('textarea');
        ta.value = text;
        ta.setAttribute('readonly', '');
        ta.style.position = 'fixed';
        ta.style.top = '-1000px';
        ta.style.opacity = '0';
        document.body.appendChild(ta);
        var sel = document.getSelection();
        var prevRange = sel && sel.rangeCount > 0 ? sel.getRangeAt(0) : null;
        ta.select();
        ta.setSelectionRange(0, text.length);
        ok = document.execCommand('copy');
        document.body.removeChild(ta);
        if (prevRange && sel) { sel.removeAllRanges(); sel.addRange(prevRange); }
    } catch (_) { ok = false; }
    if (ok) return Promise.resolve(true);
    if (navigator.clipboard && navigator.clipboard.writeText) {
        return navigator.clipboard.writeText(text).then(
            function() { return true; },
            function() { return false; }
        );
    }
    return Promise.resolve(false);
};

var _rsAndroidMediaPermissionSeq = 0;
var _rsAndroidMediaPermissionWaiters = {};

window._onAndroidMediaPermissionResult = function(data) {
    data = data || {};
    var requestId = data.request_id || '';
    var waiter = _rsAndroidMediaPermissionWaiters[requestId];
    if (!waiter) return;
    delete _rsAndroidMediaPermissionWaiters[requestId];
    waiter(!!data.granted);
};

function _rsStopMediaStream(stream) {
    if (!stream || typeof stream.getTracks !== 'function') return;
    stream.getTracks().forEach(function(track) {
        try { track.stop(); } catch (_) {}
    });
}

function _rsAndroidMediaPermission(audio, camera) {
    return new Promise(function(resolve) {
        if (!hasAndroidBridge()
            || typeof window.RatspeakAndroid.hasMediaPermissions !== 'function'
            || typeof window.RatspeakAndroid.requestMediaPermissions !== 'function') {
            resolve(null);
            return;
        }
        try {
            if (window.RatspeakAndroid.hasMediaPermissions(!!audio, !!camera)) {
                resolve(true);
                return;
            }
            var requestId = 'media-' + Date.now() + '-' + (++_rsAndroidMediaPermissionSeq);
            _rsAndroidMediaPermissionWaiters[requestId] = resolve;
            window.RatspeakAndroid.requestMediaPermissions(!!audio, !!camera, requestId);
            setTimeout(function() {
                if (!_rsAndroidMediaPermissionWaiters[requestId]) return;
                delete _rsAndroidMediaPermissionWaiters[requestId];
                resolve(false);
            }, 30000);
        } catch (err) {
            window.RS.diag('warn', '[media] Android permission bridge failed:', err);
            resolve(null);
        }
    });
}

function _rsBrowserMediaPermission(audio, camera) {
    if (!navigator.mediaDevices || typeof navigator.mediaDevices.getUserMedia !== 'function') {
        return Promise.resolve(null);
    }
    var constraints = {};
    if (audio) constraints.audio = true;
    if (camera) constraints.video = true;
    if (!constraints.audio && !constraints.video) return Promise.resolve(true);
    return navigator.mediaDevices.getUserMedia(constraints).then(function(stream) {
        _rsStopMediaStream(stream);
        return true;
    }).catch(function(err) {
        window.RS.diag('warn', '[media] getUserMedia permission probe failed:', err);
        return false;
    });
}

function _rsNativeMicrophonePermission(audio) {
    var nativeApple = typeof isTauriDesktop === 'function' && isTauriDesktop();
    if (!nativeApple && typeof isTauriMobile === 'function' && isTauriMobile() &&
        typeof isIOS === 'function' && isIOS()) {
        nativeApple = true;
    }
    if (!audio || !nativeApple) {
        return Promise.resolve(null);
    }
    if (!window.RS || typeof RS.invoke !== 'function') {
        return Promise.resolve(null);
    }
    return RS.invoke('request_microphone_permission').then(function(granted) {
        return !!granted;
    }).catch(function(err) {
        window.RS.diag('warn', '[media] native microphone permission probe failed:', err);
        return null;
    });
}

window.RS.mediaPermissions = {
    ensure: function(opts) {
        opts = opts || {};
        var audio = !!opts.audio;
        var camera = !!opts.camera;
        if (!audio && !camera) return Promise.resolve(true);
        return _rsAndroidMediaPermission(audio, camera).then(function(androidGranted) {
            if (androidGranted !== null) return androidGranted;
            return _rsNativeMicrophonePermission(audio).then(function(nativeMicGranted) {
                if (nativeMicGranted === false) return false;
                return _rsBrowserMediaPermission(audio && nativeMicGranted !== true, camera).then(function(browserGranted) {
                    return browserGranted !== false;
                });
            });
        });
    }
};

var _rsAudioPlaybackContext = null;
var _rsAudioPlaybackUnlockInstalled = false;
var _rsAudioPlaybackUnlockInFlight = false;
var _rsAudioPlaybackUnlocked = false;
var _rsAudioPlaybackPrimed = false;
var _rsAudioPlaybackUnlockEvents = ['pointerdown', 'touchend', 'mousedown', 'keydown'];

function _rsNativeAndroidAudioAvailable() {
    return !!(window.RatspeakAndroid &&
        typeof window.RatspeakAndroid.playCallRingtone === 'function' &&
        typeof window.RatspeakAndroid.startCallAudioRoute === 'function');
}

function _rsAudioPlaybackCtor() {
    return window.AudioContext || window.webkitAudioContext || null;
}

function _rsGetAudioPlaybackContext() {
    if (_rsNativeAndroidAudioAvailable()) return null;
    var ctor = _rsAudioPlaybackCtor();
    if (!ctor) return null;
    if (_rsAudioPlaybackContext && _rsAudioPlaybackContext.state !== 'closed') {
        return _rsAudioPlaybackContext;
    }
    if (_rsAudioPlaybackContext && _rsAudioPlaybackContext.state === 'closed') {
        _rsAudioPlaybackContext = null;
        _rsAudioPlaybackUnlocked = false;
        _rsAudioPlaybackPrimed = false;
    }
    try {
        _rsAudioPlaybackContext = new ctor();
    } catch (err) {
        window.RS.diag('warn', '[audio] playback context unavailable:', err);
        return null;
    }
    return _rsAudioPlaybackContext;
}

function _rsPrimeAudioPlayback(ctx) {
    if (!ctx || _rsAudioPlaybackPrimed) return;
    _rsAudioPlaybackPrimed = true;
    try {
        var gain = ctx.createGain();
        gain.gain.setValueAtTime(0, ctx.currentTime);
        gain.connect(ctx.destination);
        var osc = ctx.createOscillator();
        osc.type = 'sine';
        osc.frequency.setValueAtTime(220, ctx.currentTime);
        osc.connect(gain);
        osc.start(ctx.currentTime);
        osc.stop(ctx.currentTime + 0.025);
        osc.onended = function() {
            try { gain.disconnect(); } catch (_) {}
        };
    } catch (err) {
        _rsAudioPlaybackPrimed = false;
        window.RS.diag('warn', '[audio] playback priming failed:', err);
    }
}

function _rsInstallAudioPlaybackUnlock() {
    // Android call tones and LXST output use native AudioTracks. Creating a
    // Web Audio destination there would keep a second, silent output path
    // alive and can make the speaker re-open when the Activity resumes.
    if (_rsNativeAndroidAudioAvailable() || _rsAudioPlaybackUnlockInstalled) return;
    _rsAudioPlaybackUnlockInstalled = true;
    var unlock = function() {
        if (_rsAudioPlaybackUnlockInFlight) return;
        _rsAudioPlaybackUnlockInFlight = true;
        window.RS.audioPlayback.ensure({ installUnlock: false }).then(function(ok) {
            _rsAudioPlaybackUnlockInFlight = false;
            if (!ok) return;
            _rsAudioPlaybackUnlockEvents.forEach(function(eventName) {
                document.removeEventListener(eventName, unlock, true);
            });
        }).catch(function() {
            _rsAudioPlaybackUnlockInFlight = false;
        });
    };
    _rsAudioPlaybackUnlockEvents.forEach(function(eventName) {
        document.addEventListener(eventName, unlock, true);
    });
}

function _rsEnsureAudioPlayback(opts) {
    opts = opts || {};
    if (opts.installUnlock !== false) _rsInstallAudioPlaybackUnlock();
    var ctx = _rsGetAudioPlaybackContext();
    if (!ctx) return Promise.resolve(false);
    var needsResume = ctx.state === 'suspended' || ctx.state === 'interrupted';
    var resume = (needsResume && typeof ctx.resume === 'function')
        ? ctx.resume()
        : Promise.resolve();
    return Promise.resolve(resume).then(function() {
        _rsPrimeAudioPlayback(ctx);
        _rsAudioPlaybackUnlocked = ctx.state === 'running';
        var ready = _rsAudioPlaybackUnlocked ||
            (ctx.state !== 'suspended' && ctx.state !== 'interrupted' && ctx.state !== 'closed');
        if (ready) {
            try { document.dispatchEvent(new CustomEvent('rs-audio-playback-ready')); } catch (_) {}
        }
        return ready;
    }).catch(function(err) {
        window.RS.diag('warn', '[audio] playback permission/unlock failed:', err);
        return false;
    });
}

window.RS.audioPlayback = {
    ensure: _rsEnsureAudioPlayback,
    installUnlock: _rsInstallAudioPlaybackUnlock,
    context: _rsGetAudioPlaybackContext,
    isReady: function() {
        var ctx = _rsAudioPlaybackContext;
        return !!(_rsAudioPlaybackUnlocked || (ctx &&
            ctx.state !== 'suspended' &&
            ctx.state !== 'interrupted' &&
            ctx.state !== 'closed'));
    }
};

function pathCountSummary(stats) {
    stats = stats || {};
    var visible = Array.isArray(stats.path_table) ? stats.path_table.length : 0;
    var total = typeof stats.path_table_total === 'number' && isFinite(stats.path_table_total)
        ? stats.path_table_total
        : visible;
    if (total < visible) total = visible;
    var truncated = !!stats.path_table_truncated && total > visible;
    return {
        visible: visible,
        total: total,
        truncated: truncated,
        label: truncated ? (visible + ' of ' + total) : String(total),
    };
}

// Tauri event bridge. Returns Promise<unlisten-fn>; await before assuming
// the handler is live. Required subscriptions reject with a static error so
// callers such as Activity can retry a late iOS bridge instead of mistaking a
// no-op unlisten function for a successfully installed listener.
window.RS.listen = function(eventName, handler, options) {
    options = options || {};
    function attach() {
        return window.__TAURI__.event.listen(eventName, function(ev) {
            try { handler(ev && ev.payload); } catch (e) { window.RS.diag('error', '[RS.listen]', eventName, e); }
        });
    }
    if (window.__TAURI__ && window.__TAURI__.event && typeof window.__TAURI__.event.listen === 'function') {
        return attach();
    }
    return new Promise(function(resolve, reject) {
        var attempts = 0;
        var iv = setInterval(function() {
            attempts++;
            if (window.__TAURI__ && window.__TAURI__.event && typeof window.__TAURI__.event.listen === 'function') {
                clearInterval(iv);
                resolve(attach());
            } else if (attempts >= 20) {
                clearInterval(iv);
                if (options.required === true) {
                    var err = new Error('Required Tauri event bridge is unavailable');
                    err.code = 'event_bridge_unavailable';
                    reject(err);
                } else {
                    window.RS.diag('warn', '[RS.listen] Tauri event bridge never appeared, dropping subscription:', eventName);
                    resolve(function() {});
                }
            }
        }, 50);
    });
};

function _rsRawIpcBytes(value) {
    if (value instanceof Uint8Array) return value;
    if (value instanceof ArrayBuffer) return new Uint8Array(value);
    if (ArrayBuffer.isView(value)) {
        return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
    }
    throw new Error('Attachment chunk response was not binary');
}

// Fetch an LXMF file attachment as bounded raw IPC chunks. This deliberately
// avoids retaining whole-file base64/JSON strings in either WebView or Rust.
// Caller must URL.revokeObjectURL when done — `RS.saveFile` does this on a timer.
window.RS.fileMetadata = function(storedName) {
    return window.RS.invoke('api_file_metadata', { storedName: storedName });
};

var _rsFileDownloadInFlight = {};
var _rsLargeFileDownloadActive = false;
var _rsSmallFileDownloadBytes = 0;

window.RS.fileDownload = function(storedName, knownMeta) {
    if (_rsFileDownloadInFlight[storedName]) return _rsFileDownloadInFlight[storedName];
    var metadata = knownMeta
        ? Promise.resolve(knownMeta)
        : window.RS.fileMetadata(storedName);
    var lane = null;
    var admittedBytes = 0;
    var download = metadata.then(function(meta) {
        var size = Number(meta && meta.size) || 0;
        if (size > (1024 * 1024 - 1)) {
            if (_rsLargeFileDownloadActive) {
                var busy = new Error('Another large attachment is being loaded');
                busy.code = 'attachment_busy';
                throw busy;
            }
            _rsLargeFileDownloadActive = true;
            lane = 'large';
        } else {
            if (_rsSmallFileDownloadBytes + size > 8 * 1024 * 1024) {
                var pressure = new Error('Attachment memory budget is currently full');
                pressure.code = 'attachment_memory_pressure';
                throw pressure;
            }
            _rsSmallFileDownloadBytes += size;
            admittedBytes = size;
            lane = 'small';
        }
        var chunkBytes = Math.min(Number(meta && meta.chunk_bytes) || (256 * 1024), 256 * 1024);
        var chunks = [];
        var offset = 0;

        function readNext() {
            if (offset >= size) return Promise.resolve();
            var length = Math.min(chunkBytes, size - offset);
            return window.RS.invoke('api_file_read_chunk', {
                storedName: storedName,
                offset: offset,
                length: length,
            }).then(function(raw) {
                var bytes = _rsRawIpcBytes(raw);
                if (bytes.byteLength !== length) throw new Error('Attachment chunk was truncated');
                chunks.push(bytes);
                offset += length;
                return readNext();
            });
        }

        return readNext().then(function() {
            var mime = (meta && meta.mime) || 'application/octet-stream';
            var blob = new Blob(chunks, { type: mime });
            return {
                url: URL.createObjectURL(blob),
                blob: blob,
                size: blob.size,
                filename: (meta && meta.filename) || storedName,
                mime: mime,
            };
        });
    }).finally(function() {
        if (lane === 'large') _rsLargeFileDownloadActive = false;
        if (lane === 'small') {
            _rsSmallFileDownloadBytes = Math.max(0, _rsSmallFileDownloadBytes - admittedBytes);
        }
        if (_rsFileDownloadInFlight[storedName] === download) {
            delete _rsFileDownloadInFlight[storedName];
        }
    });
    _rsFileDownloadInFlight[storedName] = download;
    return download;
};

var _rsAndroidFileSaveSeq = 0;
var _rsAndroidFileSaveWaiters = {};

window._onAndroidFileSaveResult = function(data) {
    data = data || {};
    var requestId = data.request_id || '';
    var waiter = _rsAndroidFileSaveWaiters[requestId];
    if (!waiter) return;
    delete _rsAndroidFileSaveWaiters[requestId];
    if (data.success) {
        waiter.resolve(data);
    } else {
        var err = new Error(data.error || 'Save failed');
        err.code = 'native_save_failed';
        waiter.reject(err);
    }
};

function _rsNativeAndroidSave(file, opts) {
    opts = opts || {};
    if (!hasAndroidBridge() || !file.data_base64) return null;
    var bridge = window.RatspeakAndroid;
    var image = /^image\//i.test(file.mime || '');
    var method = image && opts.preferPhotos && typeof bridge.saveImageToPhotos === 'function'
        ? 'saveImageToPhotos'
        : (typeof bridge.saveFileDocument === 'function' ? 'saveFileDocument' : null);
    if (!method) return null;
    return new Promise(function(resolve, reject) {
        var requestId = 'file-save-' + Date.now() + '-' + (++_rsAndroidFileSaveSeq);
        _rsAndroidFileSaveWaiters[requestId] = { resolve: resolve, reject: reject };
        try {
            bridge[method](file.filename || 'download', file.data_base64 || '', file.mime || 'application/octet-stream', requestId);
        } catch (err) {
            delete _rsAndroidFileSaveWaiters[requestId];
            reject(err);
        }
        setTimeout(function() {
            if (!_rsAndroidFileSaveWaiters[requestId]) return;
            delete _rsAndroidFileSaveWaiters[requestId];
            var timeout = new Error('Save timed out');
            timeout.code = 'native_save_timeout';
            reject(timeout);
        }, 60000);
    });
}

function _rsNativeIosSavePhoto(file, opts) {
    opts = opts || {};
    if (!isIOS() || !file.data_base64 || !opts.preferPhotos || !/^image\//i.test(file.mime || '')) return null;
    if (typeof window.RS.invoke !== 'function') return null;
    return window.RS.invoke('save_image_to_photos', {
        filename: file.filename || 'image',
        mime: file.mime || 'image/png',
        dataBase64: file.data_base64 || ''
    });
}

function _rsSaveStoredFileNative(storedName, opts) {
    if (!(isTauriMobile() || isTauriDesktop()) || typeof window.RS.invoke !== 'function') return null;
    opts = opts || {};
    var requestId = 'stored-save-' + Date.now() + '-' + (++_rsAndroidFileSaveSeq);
    var callbackPromise = null;
    if (isAndroid()) {
        callbackPromise = new Promise(function(resolve, reject) {
            _rsAndroidFileSaveWaiters[requestId] = { resolve: resolve, reject: reject };
            setTimeout(function() {
                if (!_rsAndroidFileSaveWaiters[requestId]) return;
                delete _rsAndroidFileSaveWaiters[requestId];
                var timeout = new Error('Save timed out');
                timeout.code = 'native_save_timeout';
                reject(timeout);
            }, 60000);
        });
    }
    return window.RS.invoke('save_stored_attachment_native', {
        storedName: storedName,
        preferPhotos: !!opts.preferPhotos,
        requestId: requestId,
    }).then(function(status) {
        if (status === 'complete') return true;
        if (status === 'cancelled') return false;
        if (status === 'pending' && callbackPromise) return callbackPromise;
        delete _rsAndroidFileSaveWaiters[requestId];
        var unsupported = new Error('Native stored-file save is unavailable');
        unsupported.code = 'native_save_unsupported';
        throw unsupported;
    }, function(error) {
        delete _rsAndroidFileSaveWaiters[requestId];
        throw error;
    });
}

function _rsShareFile(file) {
    if (!navigator.share || typeof File === 'undefined') return null;
    try {
        if (!file.blob) return null;
        var shareFile = new File([file.blob], file.filename || 'download', {
            type: file.mime || 'application/octet-stream'
        });
        if (navigator.canShare && !navigator.canShare({ files: [shareFile] })) return null;
        return navigator.share({ files: [shareFile], title: file.filename || 'Ratspeak file' });
    } catch (_) {
        return null;
    }
}

// Blob URL is auto-revoked after 60s.
window.RS.saveDownloadedFile = function(file, opts) {
    opts = opts || {};
    var nativeAndroid = _rsNativeAndroidSave(file, opts);
    if (nativeAndroid) return nativeAndroid;
    var nativeIosPhoto = _rsNativeIosSavePhoto(file, opts);
    if (nativeIosPhoto) return nativeIosPhoto;
    if (isTauriMobile()) {
        var shared = _rsShareFile(file);
        if (shared) return shared;
    }
    return Promise.resolve().then(function() {
        var a = document.createElement('a');
        a.href = file.url;
        a.download = file.filename || 'download';
        a.style.display = 'none';
        document.body.appendChild(a);
        a.click();
        a.remove();
        setTimeout(function() { try { URL.revokeObjectURL(file.url); } catch (_) {} }, 60000);
        return file;
    });
};

window.RS.saveFile = function(storedName, opts) {
    var nativeSave = _rsSaveStoredFileNative(storedName, opts || {});
    if (nativeSave) {
        return nativeSave.catch(function(error) {
            if (!error || error.code !== 'native_save_unsupported') throw error;
            return window.RS.fileDownload(storedName).then(function(f) {
                return window.RS.saveDownloadedFile(f, opts || {});
            });
        });
    }
    return window.RS.fileDownload(storedName).then(function(f) {
        return window.RS.saveDownloadedFile(f, opts || {});
    });
};

window.RS.openExternalUrl = function(url) {
    if (!url) return Promise.resolve(false);
    var clean = String(url).trim();
    if (!/^https?:\/\//i.test(clean)) clean = 'https://' + clean;
    if (hasAndroidBridge() && typeof window.RatspeakAndroid.openExternalUrl === 'function') {
        try {
            return Promise.resolve(!!window.RatspeakAndroid.openExternalUrl(clean));
        } catch (error) {
            return Promise.reject(error);
        }
    }
    if (_rsInvokeAvailable() || isTauriMobile() || isTauriDesktop()) {
        return window.RS.invoke('open_external_url', { url: clean }).then(function() { return true; });
    }
    try {
        var opened = window.open(clean, '_blank');
        if (opened) opened.opener = null;
        return Promise.resolve(!!opened);
    } catch (error) {
        return Promise.reject(error);
    }
};

window.RS.openSupportEmail = function(subject, body) {
    var cleanSubject = String(subject || '').trim();
    var cleanBody = String(body || '');
    if (!cleanSubject || cleanSubject.length > 180 || cleanBody.length > 8000) {
        return Promise.reject(new Error('Invalid support email'));
    }
    if (hasAndroidBridge() && typeof window.RatspeakAndroid.openSupportEmail === 'function') {
        try {
            return Promise.resolve(!!window.RatspeakAndroid.openSupportEmail(cleanSubject, cleanBody));
        } catch (_) {}
    }
    if (typeof window.RS.invoke === 'function') {
        return window.RS.invoke('open_support_email', {
            subject: cleanSubject,
            body: cleanBody
        }).then(function() { return true; }).catch(function() { return false; });
    }
    return Promise.resolve(false);
};

// Native notifications share one app preference. Android permission recovery
// uses its Activity bridge; iOS and desktop use Tauri's notification plugin.
var _desktopNotifEnabled = true;
var _mobileNotificationPermissionKey = 'ratspeak_notification_permission_requested';

function _mobileNotificationPermissionWasRequested() {
    try { return localStorage.getItem(_mobileNotificationPermissionKey) === '1'; }
    catch (_) { return false; }
}

function _markMobileNotificationPermissionRequested() {
    try { localStorage.setItem(_mobileNotificationPermissionKey, '1'); }
    catch (_) {}
}

function _rsNotifyInvoke(cmd, payload) {
    return window.__TAURI_INTERNALS__.invoke('plugin:notification|' + cmd, payload || {});
}

// Mobile-only; desktop falls back to navigator.vibrate (no-op on macOS/Windows).
function _rsHapticsInvoke(method, payload) {
    var commandMap = {
        impactFeedback: 'impact_feedback',
        notificationFeedback: 'notification_feedback',
        selectionFeedback: 'selection_feedback'
    };
    var command = commandMap[method] || method;
    return window.__TAURI_INTERNALS__.invoke('plugin:haptics|' + command, payload || {});
}

window.rsNotify = {
    setEnabled: function(enabled) { _desktopNotifEnabled = !!enabled; },
    isEnabled: function() { return _desktopNotifEnabled; },
    available: function() {
        if (hasAndroidBridge()) return true;
        if (window.__TAURI_INTERNALS__) return true;
        return 'Notification' in window;
    },
    permissionState: function() {
        if (hasAndroidBridge() && typeof window.RatspeakAndroid.notificationAuthorizationStatus === 'function') {
            try { return Promise.resolve(window.RatspeakAndroid.notificationAuthorizationStatus()); }
            catch (_) { return Promise.resolve('unavailable'); }
        }
        if (window.__TAURI_INTERNALS__) {
            return _rsNotifyInvoke('is_permission_granted').then(function(granted) {
                if (granted === true) return 'granted';
                if (granted === false && isTauriMobile()) {
                    return _mobileNotificationPermissionWasRequested() ? 'denied' : 'prompt';
                }
                return granted === false ? 'denied' : 'prompt';
            }).catch(function() { return 'unavailable'; });
        }
        if ('Notification' in window) return Promise.resolve(Notification.permission);
        return Promise.resolve('unavailable');
    },
    requestPermission: function() {
        if (hasAndroidBridge() && typeof window.RatspeakAndroid.requestNotificationPermission === 'function') {
            try {
                _markMobileNotificationPermissionRequested();
                window.RatspeakAndroid.requestNotificationPermission();
            }
            catch (_) { return Promise.resolve('unavailable'); }
            return Promise.resolve('prompt');
        }
        if (window.__TAURI_INTERNALS__) {
            return _rsNotifyInvoke('is_permission_granted').then(function(granted) {
                if (granted === true) return 'granted';
                if (granted === false && (!isTauriMobile() || _mobileNotificationPermissionWasRequested())) {
                    return 'denied';
                }
                if (isTauriMobile()) _markMobileNotificationPermissionRequested();
                return _rsNotifyInvoke('request_permission');
            }).catch(function(err) {
                window.RS.diag('warn', '[rsNotify] permission probe failed:', err);
                return 'default';
            });
        }
        if ('Notification' in window) {
            if (Notification.permission !== 'default') return Promise.resolve(Notification.permission);
            return Notification.requestPermission();
        }
        return Promise.resolve('default');
    },
    send: function(opts) {
        if (!opts || !opts.title) return;
        if (isTauriMobile()) return;
        if (!_desktopNotifEnabled) return;
        if (window.__TAURI_INTERNALS__) {
            var payload = { options: { title: String(opts.title), body: String(opts.body || '') } };
            _rsNotifyInvoke('notify', payload).catch(function(err) {
                window.RS.diag('warn', '[rsNotify] notify failed:', err);
            });
            return;
        }
        if ('Notification' in window && Notification.permission === 'granted') {
            var notif = new Notification(opts.title, {
                body: opts.body || '',
                tag: opts.tag || undefined,
                silent: false,
            });
            if (typeof opts.onClick === 'function') {
                notif.onclick = function() {
                    window.focus();
                    try { opts.onClick(); } catch (_) {}
                    notif.close();
                };
            }
        }
    }
};

function disableAutoCorrect(el) {
    el.setAttribute('autocorrect', 'off');
    el.setAttribute('autocapitalize', 'none');
    el.setAttribute('spellcheck', 'false');
}

var _cachedIdentityHash = localStorage.getItem('ratspeak_identity_hash') || '';
var _cachedIdentityName = localStorage.getItem('ratspeak_identity_name') || '';

var events = [];
var MAX_EVENTS = 200;
var lastStats = null;
var activeLogFilter = 'all';
// null = no stats received yet (allow backend gate); false = confirmed zero online.
var _anyInterfaceOnline = null;

var _connectionsHasRendered = false;
var _connectionsRenderScheduled = false;
var _connectionsThrottleTimer = null;
var _connectionsFirstLoadTimer = null;

// Set when data changes while a view is inactive; consumed on view entry.
var _viewDirty = {};
function markViewDirty(viewId) { _viewDirty[viewId] = true; }
function clearViewDirty(viewId) { delete _viewDirty[viewId]; }
function isViewActive(viewId) {
    var el = document.getElementById('view-' + viewId);
    return el && el.classList.contains('active');
}

var nodeNames = {};

var lxmfIdentityHash = null;

function prettySize(num) {
    if (num === null || num === undefined) return '\u2014';
    if (num < 1000) return num + ' B';
    if (num < 1000000) return Math.round(num / 1000) + ' KB';
    if (num < 1000000000) return (num / 1000000).toFixed(1) + ' MB';
    return (num / 1000000000).toFixed(2) + ' GB';
}

function prettySpeed(bps) {
    if (bps === null || bps === undefined || bps === 0) return '0 bps';
    if (bps < 1000) return bps.toFixed(0) + ' bps';
    if (bps < 1000000) return (bps / 1000).toFixed(1) + ' Kbps';
    if (bps < 1000000000) return (bps / 1000000).toFixed(2) + ' Mbps';
    return (bps / 1000000000).toFixed(2) + ' Gbps';
}

function prettyBitrate(bps) {
    if (bps === null || bps === undefined) return '\u2014';
    return prettySpeed(bps);
}

function prettyTime(seconds) {
    if (seconds === null || seconds === undefined) return '\u2014';
    var d = Math.floor(seconds / 86400);
    var h = Math.floor((seconds % 86400) / 3600);
    var m = Math.floor((seconds % 3600) / 60);
    var s = Math.floor(seconds % 60);
    var pad = function(n) { return n < 10 ? '0' + n : '' + n; };
    if (d > 0) return d + 'd ' + pad(h) + 'h';
    if (h > 0) return h + 'h ' + pad(m) + 'm';
    if (m > 0) return m + 'm ' + pad(s) + 's';
    return pad(s) + 's';
}

var _use12Hour = (function() {
    try {
        var hc = new Intl.DateTimeFormat(undefined, { hour: 'numeric' }).resolvedOptions().hourCycle;
        return hc === 'h12' || hc === 'h11';
    } catch (e) { return false; }
})();

var _dateOrderDMY = (function() {
    try {
        var parts = new Intl.DateTimeFormat(undefined, { day: 'numeric', month: 'numeric' }).formatToParts(new Date(2000, 0, 15));
        for (var i = 0; i < parts.length; i++) {
            if (parts[i].type === 'day') return true;
            if (parts[i].type === 'month') return false;
        }
    } catch (e) {}
    return false;
})();

function formatTime(ts) {
    if (!ts) return '';
    return _formatClockMinute(new Date(ts * 1000));
}

function formatConvTime(ts) {
    if (!ts) return '';
    var d = new Date(ts * 1000);
    var now = new Date();
    var today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
    var msgDay = new Date(d.getFullYear(), d.getMonth(), d.getDate());
    var diffDays = Math.round((today - msgDay) / 86400000);
    if (diffDays === 0) {
        var m = d.getMinutes().toString().padStart(2, '0');
        if (_use12Hour) {
            var h = d.getHours();
            var period = h >= 12 ? 'PM' : 'AM';
            h = h % 12 || 12;
            return h + ':' + m + ' ' + period;
        }
        return d.getHours().toString().padStart(2, '0') + ':' + m;
    }
    if (diffDays === 1) return 'yesterday';
    if (diffDays <= 7) return diffDays + 'd';
    var dd = d.getDate().toString().padStart(2, '0');
    var mm = (d.getMonth() + 1).toString().padStart(2, '0');
    var yyyy = d.getFullYear();
    if (yyyy === now.getFullYear()) return _dateOrderDMY ? dd + '/' + mm : mm + '/' + dd;
    return _dateOrderDMY ? dd + '/' + mm + '/' + yyyy : mm + '/' + dd + '/' + yyyy;
}

function _formatClockMinute(d) {
    var m = d.getMinutes().toString().padStart(2, '0');
    if (_use12Hour) {
        var h = d.getHours();
        var period = h >= 12 ? 'PM' : 'AM';
        h = h % 12 || 12;
        return h + ':' + m + ' ' + period;
    }
    return d.getHours().toString().padStart(2, '0') + ':' + m;
}

function formatLastHeard(ts) {
    if (!ts) return 'No activity yet';
    var d = new Date(ts * 1000);
    var now = new Date();
    var diffSecs = Math.max(0, Math.floor((now.getTime() - d.getTime()) / 1000));
    if (diffSecs < 60) return 'just now';
    if (diffSecs < 3600) {
        var mins = Math.max(1, Math.floor(diffSecs / 60));
        return mins + ' minute' + (mins === 1 ? '' : 's') + ' ago';
    }

    var today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
    var heardDay = new Date(d.getFullYear(), d.getMonth(), d.getDate());
    var diffDays = Math.round((today - heardDay) / 86400000);
    var time = _formatClockMinute(d);
    if (diffDays === 0) return 'today at ' + time;
    if (diffDays === 1) return 'yesterday at ' + time;
    if (diffDays < 7) {
        return d.toLocaleDateString(undefined, { weekday: 'long' }) + ' at ' + time;
    }
    if (d.getFullYear() === now.getFullYear()) {
        return d.toLocaleDateString(undefined, { month: 'short', day: 'numeric' }) + ' at ' + time;
    }
    return d.toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: 'numeric' });
}

function friendlyNode(nodeName) {
    if (!nodeName) return 'Unknown';
    if (nodeNames[nodeName]) return nodeNames[nodeName];
    return nodeName.replace('node_', 'Node ');
}

function escapeHtml(str) {
    if (!str) return '';
    return str.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;').replace(/'/g, '&#39;');
}

function shortHash(fullHash, front, back) {
    if (!fullHash) return '';
    front = front || 8;
    back = back || 4;
    if (fullHash.length <= front + back + 1) return fullHash;
    return fullHash.substring(0, front) + '\u2026' + fullHash.slice(-back);
}

function copyableHash(fullHash, displayLength) {
    if (!fullHash) return '<span class="hash-copy">&mdash;</span>';
    var displayText = fullHash;
    if (displayLength && fullHash.length > displayLength) {
        displayText = shortHash(fullHash, displayLength, 4);
    }
    return '<button class="hash-copy" type="button" dir="ltr" data-full="' + escapeHtml(fullHash) +
        '" aria-label="Copy address ' + escapeHtml(fullHash) +
        '" title="Copy ' + escapeHtml(fullHash) + '">' +
        escapeHtml(displayText) + '</button>';
}

function debounce(fn, delay) {
    var timer = null;
    return function() {
        var ctx = this, args = arguments;
        if (timer) clearTimeout(timer);
        timer = setTimeout(function() { fn.apply(ctx, args); }, delay);
    };
}

// Dashboard is a desktop quickview, not a destination in the native/compact
// tab bar. Native WebViews can briefly report a wide viewport during first
// layout, so the injected platform flag is authoritative and width is only a
// browser/responsive fallback.
function appUsesMobileNavigation() {
    var nativeMobile = typeof isTauriMobile === 'function' && isTauriMobile();
    var compact = typeof isCompactLayout === 'function' && isCompactLayout();
    return nativeMobile || compact;
}

function appLandingView() {
    return appUsesMobileNavigation() ? 'peers' : 'dashboard';
}

function reloadToAppLanding() {
    window.location.href = '/#' + appLandingView();
    window.location.reload();
}

function waitForServerAndReload(maxRetries, targetPath) {
    maxRetries = maxRetries || 30;
    var target = targetPath || '/#' + appLandingView();
    var attempt = 0;
    function getDelay() {
        if (attempt <= 10) return 1000;
        if (attempt <= 20) return 1500;
        return 2000;
    }
    function tryReload() {
        attempt++;
        RS.invoke('api_version').then(function() {
            window.location.href = target;
            window.location.reload();
        }).catch(function() {
            if (attempt < maxRetries) {
                setTimeout(tryReload, getDelay());
            } else {
                window.location.href = target;
                window.location.reload();
            }
        });
    }
    // Cover port release + process restart + hub startup.
    setTimeout(tryReload, 6000);
}

document.addEventListener('click', function(e) {
    var target = e.target.closest('.hash-copy');
    if (!target || !target.dataset.full) return;

    e.stopPropagation();
    var fullHash = target.dataset.full;
    RS.copyText(fullHash).then(function(ok) {
        if (ok) {
            showCopyConfirmationToast('Address');
            target.classList.add('copied');
            setTimeout(function() { target.classList.remove('copied'); }, 850);
        } else {
            target.textContent = fullHash;
            target.title = 'Address selected for manual copy';
            var range = document.createRange();
            range.selectNodeContents(target);
            var sel = window.getSelection();
            sel.removeAllRanges();
            sel.addRange(range);
        }
    });
});

function animateCountUp(element, target, duration) {
    if (!element) return;
    duration = duration || 400;
    var start = parseInt(element.textContent) || 0;
    if (start === target) return;
    var startTime = null;
    function step(timestamp) {
        if (!startTime) startTime = timestamp;
        var progress = Math.min((timestamp - startTime) / duration, 1);
        var eased = 1 - Math.pow(1 - progress, 3);
        element.textContent = Math.round(start + (target - start) * eased);
        if (progress < 1) requestAnimationFrame(step);
    }
    requestAnimationFrame(step);
}

var _lifecycleWasHidden = false;

function _postLifecycleForeground(foreground, detail) {
    if (!window.__RATSPEAK_DESKTOP__) {
        if (foreground) {
            var mobileDetail = detail || {};
            mobileDetail.foreground = true;
            try {
                document.dispatchEvent(new CustomEvent('rs-lifecycle-foreground-handled', { detail: mobileDetail }));
            } catch (_) {}
        }
        return Promise.resolve(true);
    }
    return RS.invoke('api_set_foreground', { args: { foreground: foreground } }).then(function() {
        if (foreground) {
            var eventDetail = detail || {};
            eventDetail.foreground = true;
            try {
                document.dispatchEvent(new CustomEvent('rs-lifecycle-foreground-handled', { detail: eventDetail }));
            } catch (_) {}
        }
        return true;
    }, function() { return false; });
}

function _currentLifecycleForeground() {
    if (window.__RATSPEAK_DESKTOP__) {
        return !document.hidden && document.hasFocus();
    }
    if (!window.__TAURI_INTERNALS__ && typeof document.hasFocus === 'function') {
        return !document.hidden && document.hasFocus();
    }
    return !document.hidden;
}

window.RS.isAttentionForeground = function() {
    return _currentLifecycleForeground();
};

// On Android the service keeps the process alive, so a resume doesn't
// trigger backend deltas — explicitly re-fetch on hidden→visible.
function _refreshAfterResume() {
    if (typeof loadIdentities === 'function') loadIdentities();
    if (typeof loadConversations === 'function') loadConversations();
    RS.invoke('api_announces').then(function(data) {
        if (Array.isArray(data) && typeof window !== 'undefined') {
            if (typeof announceCache !== 'undefined') announceCache = data;
            if (typeof renderAnnounceList === 'function') renderAnnounceList();
        }
    }).catch(function() {});
    if (typeof lxmfActiveContact !== 'undefined' && lxmfActiveContact) {
        RS.invoke('get_conversation', { hash: lxmfActiveContact }).catch(function() {});
    }
}

// visibilitychange doesn't fire on initial visible state.
_postLifecycleForeground(_currentLifecycleForeground(), { source: 'initial', persisted: false });
_lifecycleWasHidden = document.hidden;

function _handleLifecycleChange() {
    var foreground = _currentLifecycleForeground();
    var nowVisible = !document.hidden;
    var lifecycleHandled = _postLifecycleForeground(foreground, { source: 'lifecycle', persisted: false });
    if (!window.__RATSPEAK_DESKTOP__ && nowVisible && _lifecycleWasHidden) {
        lifecycleHandled.then(function(handled) { if (handled) _refreshAfterResume(); });
    }
    _lifecycleWasHidden = !nowVisible;
}

document.addEventListener('visibilitychange', _handleLifecycleChange);
window.addEventListener('focus', _handleLifecycleChange);
window.addEventListener('blur', _handleLifecycleChange);

if (!window.__RATSPEAK_DESKTOP__) {

    // pageshow covers mobile Safari bfcache where visibilitychange may not fire.
    window.addEventListener('pageshow', function(e) {
        if (e.persisted) {
            _postLifecycleForeground(true, { source: 'pageshow', persisted: true }).then(function(handled) {
                if (handled) _refreshAfterResume();
            });
            _lifecycleWasHidden = false;
        }
    });
}

RS.listen('attachment_memory_pressure', function(payload) {
    if (typeof handleAttachmentMemoryPressure === 'function') {
        handleAttachmentMemoryPressure(!!(payload && payload.critical));
    }
    if (window.RS && RS.voiceMemos && typeof RS.voiceMemos.releaseInactiveMedia === 'function') {
        RS.voiceMemos.releaseInactiveMedia(!!(payload && payload.critical));
    }
}).catch(function() {});
