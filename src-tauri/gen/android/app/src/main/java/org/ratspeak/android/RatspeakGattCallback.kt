package org.ratspeak.android

import android.annotation.SuppressLint
import android.bluetooth.*

/**
 * BluetoothGattServer callback for the Bluetooth Peer peripheral role.
 *
 * Handles incoming connections from other Ratspeak/Columba devices,
 * serves the identity characteristic, and forwards received data
 * to the Rust layer via registered JNI native methods.
 */
@SuppressLint("MissingPermission")
class RatspeakGattCallback(
    private val gattServer: () -> BluetoothGattServer?
) : BluetoothGattServerCallback() {

    companion object {
        private const val TAG = "Ratspeak"
        // Standard Client Characteristic Configuration Descriptor UUID.
        // A peer enables notifications by writing 0x01,0x00 here; disables with 0x00,0x00.
        private val CCCD_UUID = java.util.UUID.fromString("00002902-0000-1000-8000-00805f9b34fb")
    }

    override fun onConnectionStateChange(device: BluetoothDevice, status: Int, newState: Int) {
        val address = device.address ?: return
        if (newState == BluetoothProfile.STATE_CONNECTED) {
            Log.i(TAG, "GATT server: peer connected $address")
            // Register the device handle so RatspeakBleServer.notifyTx can
            // resolve a string address back to the live BluetoothDevice when
            // Rust pushes outbound notifications.
            RatspeakBleServer.recordConnection(device)
            nativeBleGattPeerConnected(address)
        } else if (newState == BluetoothProfile.STATE_DISCONNECTED) {
            Log.i(TAG, "GATT server: peer disconnected $address")
            // Drop the device handle and any subscription state so a stale
            // address can't be picked up by a later broadcastTx.
            RatspeakBleServer.removeConnection(address)
            nativeBleGattPeerDisconnected(address)
        }
    }

    override fun onMtuChanged(device: BluetoothDevice, mtu: Int) {
        // Stash the negotiated ATT payload size (MTU - 3) so notifyTx can
        // size fragments per peer instead of the conservative default (B6).
        val addr = device.address ?: return
        if (mtu > 3) {
            RatspeakBleServer.recordMtu(addr, mtu - 3)
        }
    }

    override fun onCharacteristicReadRequest(
        device: BluetoothDevice,
        requestId: Int,
        offset: Int,
        characteristic: BluetoothGattCharacteristic
    ) {
        // No readable characteristic value: never serve the identity hash on a
        // GATT read (a MAC-rotation-stable tracking vector). Peers learn
        // identity from signed announces over the TX notify pipe instead.
        gattServer()?.sendResponse(device, requestId, BluetoothGatt.GATT_SUCCESS, offset, byteArrayOf())
    }

    override fun onCharacteristicWriteRequest(
        device: BluetoothDevice,
        requestId: Int,
        characteristic: BluetoothGattCharacteristic,
        preparedWrite: Boolean,
        responseNeeded: Boolean,
        offset: Int,
        value: ByteArray?
    ) {
        // Forward received data to Rust via JNI, tagged with the source device
        // address so the consumer can keep per-peer reassembly state and skip
        // relaying packets back to the same peer (anti-loop in B2).
        if (value != null && value.isNotEmpty()) {
            val addr = device.address ?: ""
            nativeBleGattDataReceived(addr, value)
        }
        if (responseNeeded) {
            gattServer()?.sendResponse(device, requestId, BluetoothGatt.GATT_SUCCESS, 0, null)
        }
    }

    /**
     * Handle CCCD descriptor writes — the standard mechanism for a central to
     * subscribe / unsubscribe from notifications on a TX characteristic.
     *
     * Spec: writing [0x01, 0x00] enables notifications; [0x02, 0x00] enables
     * indications (we treat the same); [0x00, 0x00] disables. Anything else
     * we record as "no subscription" for safety.
     */
    override fun onDescriptorWriteRequest(
        device: BluetoothDevice,
        requestId: Int,
        descriptor: BluetoothGattDescriptor,
        preparedWrite: Boolean,
        responseNeeded: Boolean,
        offset: Int,
        value: ByteArray?
    ) {
        val server = gattServer()
        val addr = device.address
        val parentChar = descriptor.characteristic
        if (addr != null && parentChar != null && descriptor.uuid == CCCD_UUID && value != null && value.isNotEmpty()) {
            val enable = value[0].toInt() and 0xFF
            val charUuid = parentChar.uuid.toString()
            when (enable) {
                0x01, 0x02 -> {
                    RatspeakBleServer.recordSubscription(addr, charUuid)
                    // Notify Rust so the dashboard can fire a kick-announce on
                    // this peer's first subscription (BitChat parity).
                    nativeBleGattPeerSubscribed(addr)
                }
                0x00 -> RatspeakBleServer.removeSubscription(addr, charUuid)
                else -> Log.w(TAG, "GATT: unexpected CCCD value ${value[0]} from $addr")
            }
        }
        if (responseNeeded) {
            server?.sendResponse(device, requestId, BluetoothGatt.GATT_SUCCESS, offset, value)
        }
    }

    override fun onNotificationSent(device: BluetoothDevice, status: Int) {
        // Flow control: release the per-device notify gate so the next
        // fragment can be sent. Release on failure too — the stack is done
        // with this send either way, and blocking further sends would only
        // strand the peer.
        if (status != BluetoothGatt.GATT_SUCCESS) {
            Log.w(TAG, "GATT notification send failed: status=$status")
        }
        device.address?.let { RatspeakBleServer.onNotifySent(it) }
    }

    // Native methods registered by Rust in JNI_OnLoad. The Rust extern fns
    // live in ble_peer.rs::android_peripheral. The data callback is tagged
    // with the source device address (A5).
    private external fun nativeBleGattDataReceived(deviceAddress: String, data: ByteArray)
    private external fun nativeBleGattPeerConnected(address: String)
    private external fun nativeBleGattPeerDisconnected(address: String)
    private external fun nativeBleGattPeerSubscribed(address: String)
}
