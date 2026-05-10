(function() {
    window.RS = window.RS || {};
    RS.ui = RS.ui || {};

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
            if (badge) {
                badge.textContent = transportLabels[mode] || mode;
                badge.setAttribute('data-value', mode);
            }
            return RS.invoke('set_transport_mode', {
                args: { mode: mode, network_type: currentNetworkType() }
            }).then(function() {
                return mode;
            }).catch(function(err) {
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
        if (typeof _trapFocus === 'function') _trapFocus(modal);
        return modal;
    };

    RS.ui.closeExistingSheet = function(modalId, overlayId) {
        var modal = elementRef(modalId);
        var overlay = elementRef(overlayId);
        if (!modal) return;
        if (typeof _releaseFocus === 'function') _releaseFocus(modal);
        modal.classList.remove('open');
        if (overlay) overlay.classList.remove('active');
    };

    function interfaceLiveStatus(ifaceName) {
        return typeof getInterfaceLiveStatus === 'function' ? getInterfaceLiveStatus(ifaceName) : null;
    }

    RS.ui.createInterfaceRow = function(iface, ifaceType, opts) {
        opts = opts || {};
        var row = document.createElement('div');
        row.className = 'hub-iface-row';

        var statusDot = document.createElement('span');
        statusDot.className = 'hub-iface-status';
        statusDot.dataset.ifaceName = iface.name;
        var liveData = interfaceLiveStatus(iface.name);
        if (liveData) {
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
        if (typeof isMobile === 'function' && isMobile() && typeof rsChoice === 'function' && opts.mobileSheet !== false) {
            var choices = items.filter(function(item) { return !item.separator && !item.disabled; }).map(function(item, idx) {
                return {
                    label: item.label,
                    value: idx,
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
