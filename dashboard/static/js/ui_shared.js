(function() {
    window.RS = window.RS || {};
    RS.ui = RS.ui || {};
    RS.composer = RS.composer || {};
    RS.text = RS.text || {};

    // Shared soft-keyboard continuity for every chat composer. Pointer-down on
    // a send control must not blur the textarea, otherwise Android closes and
    // reopens the IME around the asynchronous command response.
    var composerFocusState = new WeakMap();

    RS.composer.captureFocus = function(input) {
        if (!input) return;
        composerFocusState.set(input, {
            wasFocused: document.activeElement === input,
            capturedAt: Date.now()
        });
    };

    RS.composer.consumeFocus = function(input) {
        if (!input) return false;
        var focusedNow = document.activeElement === input;
        var state = composerFocusState.get(input);
        composerFocusState.delete(input);
        if (state && Date.now() - state.capturedAt < 8000) {
            return state.wasFocused || focusedNow;
        }
        return focusedNow;
    };

    RS.composer.focusWithoutScroll = function(input) {
        if (!input || document.activeElement === input) return;
        try { input.focus({ preventScroll: true }); }
        catch (_) { input.focus(); }
    };

    // Composer replacements (voice recorder, attachment review, etc.) must
    // wait for the mobile IME/visual viewport to settle after blur. Keeping
    // this transition here prevents each composer from inventing a subtly
    // different keyboard workaround.
    RS.composer.dismissForReplacement = function(input) {
        if (input && document.activeElement === input) input.blur();
        return new Promise(function(resolve) {
            var startedAt = Date.now();
            function settled() {
                var mobile = typeof isTauriMobile === 'function' && isTauriMobile();
                var keyboardOpen = document.documentElement.classList.contains('keyboard-open');
                if (!mobile || !keyboardOpen || Date.now() - startedAt >= 240) {
                    if (typeof requestAnimationFrame === 'function') {
                        requestAnimationFrame(function() { requestAnimationFrame(resolve); });
                    } else {
                        setTimeout(resolve, 0);
                    }
                    return;
                }
                setTimeout(settled, 24);
            }
            settled();
        });
    };

    RS.composer.resize = function(input, maxHeight) {
        if (!input) return '';
        input.style.height = 'auto';
        input.style.height = Math.min(input.scrollHeight, maxHeight || 124) + 'px';
        return input.style.height;
    };

    RS.composer.reset = function(input) {
        if (!input) return;
        input.value = '';
        input.style.height = '';
        input.scrollTop = 0;
    };

    RS.text.utf8Length = function(value) {
        var text = String(value == null ? '' : value);
        if (window.TextEncoder) return new TextEncoder().encode(text).length;
        return unescape(encodeURIComponent(text)).length;
    };

    RS.text.truncateUtf8 = function(value, maxBytes) {
        var result = '';
        var used = 0;
        Array.from(String(value == null ? '' : value)).some(function(character) {
            var bytes = RS.text.utf8Length(character);
            if (used + bytes > maxBytes) return true;
            result += character;
            used += bytes;
            return false;
        });
        return result;
    };

    RS.composer.bindTapToSend = function(button, input, onSend) {
        if (!button || !input || typeof onSend !== 'function' || button._rsStableSendBound) return;
        button._rsStableSendBound = true;
        var startX = 0;
        var startY = 0;
        var moved = false;
        var suppressClickUntil = 0;
        var moveCancelSq = 12 * 12;

        button.addEventListener('touchstart', function(event) {
            event.preventDefault();
            RS.composer.captureFocus(input);
            var touch = event.touches && event.touches[0];
            if (touch) {
                startX = touch.clientX;
                startY = touch.clientY;
            }
            moved = false;
        }, { passive: false });

        button.addEventListener('touchmove', function(event) {
            var touch = event.touches && event.touches[0];
            if (!touch) return;
            var dx = touch.clientX - startX;
            var dy = touch.clientY - startY;
            if (dx * dx + dy * dy > moveCancelSq) moved = true;
        }, { passive: true });

        button.addEventListener('touchend', function(event) {
            event.preventDefault();
            suppressClickUntil = Date.now() + 500;
            if (!moved) onSend();
        });

        button.addEventListener('touchcancel', function() { moved = true; });
        button.addEventListener('mousedown', function(event) {
            event.preventDefault();
            RS.composer.captureFocus(input);
        });
        button.addEventListener('click', function(event) {
            if (Date.now() < suppressClickUntil) {
                event.preventDefault();
                event.stopPropagation();
                return;
            }
            RS.composer.captureFocus(input);
            onSend();
        });
    };

    RS.ui.bindKeyboardActivation = function(element) {
        if (!element || element._ratspeakKeyboardActivationBound) return;
        element._ratspeakKeyboardActivationBound = true;
        element.addEventListener('keydown', function(event) {
            if (event.key !== 'Enter' && event.key !== ' ') return;
            event.preventDefault();
            element.click();
        });
    };

    var transportLabels = { auto: 'AUTO', on: 'ON', off: 'OFF' };
    var transportChoices = [
        { label: 'AUTO', value: 'auto', hint: 'Enables only on suitable non-LoRa interfaces.' },
        { label: 'ON', value: 'on', hint: 'Always relay packets.' },
        { label: 'OFF', value: 'off', hint: 'Never relay packets.' }
    ];

    function elementRef(elOrId) {
        return typeof elOrId === 'string' ? document.getElementById(elOrId) : elOrId;
    }

    function currentNetworkType() {
        if (navigator.connection && navigator.connection.type) return navigator.connection.type;
        if (navigator.connection && navigator.connection.effectiveType) return navigator.connection.effectiveType;
        return 'unknown';
    }

    RS.ui.applyTransportModePayload = function(elOrId, data, opts) {
        opts = opts || {};
        var mode = (data && data.mode) || 'off';
        var badge = elementRef(elOrId);
        if (badge) {
            badge.textContent = transportLabels[mode] || mode.toUpperCase();
            badge.setAttribute('data-value', mode);
        }
        if (opts.toastSuppressed && data && data.suppressed && typeof showToast === 'function') {
            showToast('Transport Mode is handled by the shared instance on this device.', 'toast-yellow', 5000);
        }
    };

    RS.ui.openTransportModeChoice = function(elOrId) {
        var badge = elementRef(elOrId);
        if (typeof rsChoice !== 'function') return Promise.resolve(null);
        return rsChoice({
            title: 'Transport Mode',
            message: 'Relay packets for other nodes on the network.',
            choices: transportChoices
        }).then(function(mode) {
            if (!mode) return null;
            var previousText = badge ? badge.textContent : '';
            var previousValue = badge ? badge.getAttribute('data-value') : '';
            if (badge) {
                badge.textContent = transportLabels[mode] || mode;
                badge.setAttribute('data-value', mode);
            }
            return RS.invoke('set_transport_mode', {
                args: { mode: mode, network_type: currentNetworkType() }
            }).then(function() {
                return mode;
            }).catch(function(err) {
                if (badge) {
                    badge.textContent = previousText || 'OFF';
                    if (previousValue) badge.setAttribute('data-value', previousValue);
                    else badge.removeAttribute('data-value');
                }
                if (typeof showToast === 'function') {
                    showToast((err && err.message) || 'Failed to update transport mode', 'toast-red', 8000);
                }
                return null;
            });
        });
    };

    RS.ui.bindTransportChoice = function(elOrId) {
        var badge = elementRef(elOrId);
        if (!badge || badge._ratspeakTransportChoiceBound) return;
        badge._ratspeakTransportChoiceBound = true;
        function openChoice() {
            RS.ui.openTransportModeChoice(badge);
        }
        badge.addEventListener('click', openChoice);
        badge.addEventListener('keydown', function(e) {
            if (e.key === 'Enter' || e.key === ' ') {
                e.preventDefault();
                openChoice();
            }
        });
    };

    RS.ui.openExistingSheet = function(modalId, overlayId) {
        var modal = elementRef(modalId);
        var overlay = elementRef(overlayId);
        if (!modal) return null;
        modal.classList.add('open');
        if (overlay) overlay.classList.add('active');
        // Feature close callbacks may clear pending edits or secrets. Never
        // replace them with the generic visual close used by simple sheets.
        if (typeof modal._ratspeakDismiss !== 'function') {
            modal._ratspeakDismiss = function() {
                RS.ui.closeExistingSheet(modal, overlay);
            };
        }
        if (!modal._ratspeakEscapeHandler) {
            modal._ratspeakEscapeHandler = function(event) {
                if (event.key === 'Escape' && modal.classList.contains('open')) {
                    event.preventDefault();
                    modal._ratspeakDismiss();
                }
            };
            document.addEventListener('keydown', modal._ratspeakEscapeHandler);
        }
        if (typeof _trapFocus === 'function' && !modal._focusTrapHandler) _trapFocus(modal);
        return modal;
    };

    RS.ui.closeExistingSheet = function(modalId, overlayId) {
        var modal = elementRef(modalId);
        var overlay = elementRef(overlayId);
        if (!modal) return;
        if (modal._ratspeakEscapeHandler) {
            document.removeEventListener('keydown', modal._ratspeakEscapeHandler);
            modal._ratspeakEscapeHandler = null;
        }
        if (typeof _releaseFocus === 'function') _releaseFocus(modal);
        modal.classList.remove('open');
        if (overlay) overlay.classList.remove('active');
    };

    function interfaceLiveStatus(ifaceName) {
        return typeof getInterfaceLiveStatus === 'function' ? getInterfaceLiveStatus(ifaceName) : null;
    }

    function interfaceConfigEnabled(iface) {
        if (!iface || typeof iface !== 'object') return true;
        var enabled = iface.enabled;
        if (enabled === undefined || enabled === null) enabled = iface.interface_enabled;
        if (enabled === undefined || enabled === null) return true;
        return !/^(false|no|0|off)$/i.test(String(enabled).trim());
    }

    RS.ui.createInterfaceRow = function(iface, ifaceType, opts) {
        opts = opts || {};
        var row = document.createElement('div');
        row.className = 'hub-iface-row';
        var paused = !interfaceConfigEnabled(iface);
        if (paused) row.classList.add('is-paused');

        var statusDot = document.createElement('span');
        statusDot.className = 'hub-iface-status';
        statusDot.dataset.ifaceName = iface.name;
        var liveData = interfaceLiveStatus(iface.name);
        if (paused) {
            statusDot.classList.add('paused');
            statusDot.title = 'Paused';
        } else if (liveData) {
            statusDot.classList.add(liveData.online ? 'up' : 'down');
            statusDot.title = liveData.online ? 'Connected' : 'Disconnected';
        } else {
            statusDot.classList.add('unknown');
            statusDot.title = 'Waiting for status...';
        }

        var nameSpan = document.createElement('span');
        nameSpan.className = 'hub-iface-name';
        nameSpan.textContent = iface.name;
        nameSpan.title = iface.name;

        var detailSpan = document.createElement('span');
        detailSpan.className = 'hub-iface-detail';
        detailSpan.textContent = typeof getIfaceDetailText === 'function' ? getIfaceDetailText(iface, ifaceType) : '';

        row.appendChild(statusDot);
        row.appendChild(nameSpan);
        row.appendChild(detailSpan);

        if (opts.actions !== false) {
            var actions = document.createElement('span');
            actions.className = 'hub-iface-actions';

            if (opts.editable && typeof isEditableInterfaceType === 'function' && isEditableInterfaceType(ifaceType)) {
                var editBtn = document.createElement('button');
                editBtn.className = 'nr-btn-sm nr-btn-muted';
                editBtn.textContent = 'Edit';
                editBtn.title = ifaceType === 'rnode' ? 'Edit radio settings' : 'Edit interface';
                editBtn.addEventListener('click', function() {
                    if (typeof openInterfaceEditModal === 'function') {
                        openInterfaceEditModal(ifaceType, iface.name, iface);
                    }
                });
                actions.appendChild(editBtn);
            }

            if (opts.removable !== false) {
                var isBleRnode = ifaceType === 'rnode' && (iface.port || '').indexOf('ble://') === 0;
                var disconnectBle = !!(opts.disconnectBle && isBleRnode);
                var removeBtn = document.createElement('button');
                removeBtn.className = 'danger-btn-sm';
                removeBtn.textContent = disconnectBle ? 'Disconnect' : 'Remove';
                removeBtn.title = disconnectBle ? 'Disconnect this device' : 'Remove this interface';
                removeBtn.addEventListener('click', function() {
                    var msg = disconnectBle ? 'Disconnect BLE LoRa radio "' + iface.name + '"?' : 'Remove "' + iface.name + '"?';
                    var confirmText = disconnectBle ? 'Disconnect' : 'Remove';
                    if (typeof rsConfirm !== 'function') return;
                    rsConfirm({ message: msg, danger: true, confirmText: confirmText }).then(function(ok) {
                        if (!ok) return;
                        if (disconnectBle) {
                            RS.invoke('disconnect_ble_rnode', { name: iface.name }).catch(function(err) {
                                if (typeof showToast === 'function') {
                                    showToast((err && err.message) || 'Failed to disconnect BLE LoRa radio', 'toast-red', 8000);
                                }
                            });
                        } else if (typeof removeHubInterface === 'function') {
                            removeHubInterface(ifaceType, iface.name);
                        }
                    });
                });
                actions.appendChild(removeBtn);
            }

            row.appendChild(actions);
        }

        return row;
    };

    RS.ui.openActionMenu = function(trigger, items, opts) {
        opts = opts || {};
        if (!trigger || !items || !items.length) return Promise.resolve(null);
        if (typeof isCompactLayout === 'function' && isCompactLayout() && typeof rsChoice === 'function' && opts.mobileSheet !== false) {
            var choices = items.filter(function(item) { return !item.separator && !item.disabled; }).map(function(item, idx) {
                return {
                    label: item.label,
                    value: idx,
                    icon: item.icon || '',
                    hint: item.hint || '',
                    danger: !!item.danger
                };
            });
            return rsChoice({ title: opts.title || 'Actions', choices: choices }).then(function(idx) {
                if (idx === null || idx === undefined) return null;
                var item = items.filter(function(candidate) { return !candidate.separator && !candidate.disabled; })[idx];
                if (item && typeof item.onSelect === 'function') item.onSelect();
                return item || null;
            });
        }
        if (typeof actionPopover === 'function') {
            actionPopover(trigger, items, opts);
        }
        return Promise.resolve(null);
    };
})();
