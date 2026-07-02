package org.ratspeak.android

import android.annotation.SuppressLint
import android.bluetooth.*
import android.content.Context
import android.os.Build
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.Semaphore
import java.util.concurrent.TimeUnit

/**
 * Singleton helper that creates and manages the BluetoothGattServer for
 * Bluetooth Peer communication. Called from Rust via JNI.
 *
 * Registers both Ratspeak (primary) and Columba (compatibility) GATT
 * services with RX, TX, and ID characteristics.
 *
 * Tracks connected centrals + their TX-characteristic subscriptions so
 * the Rust outbound fan-out (A4) can push notifications back to peers
 * that connected to *us* as Central. Without this state, the mesh is
 * one-way: we receive their writes but cannot reply through the same
 * connection.
 */
@SuppressLint("MissingPermission")
object RatspeakBleServer {
    private const val TAG = "Ratspeak"

    private var gattServer: BluetoothGattServer? = null

    // Client Configuration Descriptor (required for NOTIFY characteristics)
    private val CCCD_UUID = UUID.fromString("00002902-0000-1000-8000-00805f9b34fb")

    // Ratspeak service UUIDs
    private val RATSPEAK_SERVICE = UUID.fromString("a1b2c3d4-e5f6-4a5b-8c9d-0e1f2a3b4c5d")
    private val RATSPEAK_RX     = UUID.fromString("a1b2c3d4-e5f6-4a5b-8c9d-0e1f2a3b4c5e")
    private val RATSPEAK_TX     = UUID.fromString("a1b2c3d4-e5f6-4a5b-8c9d-0e1f2a3b4c5f")

    // Columba compatibility service UUIDs
    private val COLUMBA_SERVICE = UUID.fromString("37145b00-442d-4a94-917f-8f42c5da28e3")
    private val COLUMBA_RX      = UUID.fromString("37145b00-442d-4a94-917f-8f42c5da28e5")
    private val COLUMBA_TX      = UUID.fromString("37145b00-442d-4a94-917f-8f42c5da28e4")

    // ── Per-device tracking, populated by RatspeakGattCallback ───────────────
    //
    // connectedDevices keys by BluetoothDevice.address so notifyTx can resolve
    // a string address back to the live device handle. ConcurrentHashMap because
    // GATT callbacks run on a binder thread while notifyTx is called from
    // tokio worker threads via JNI.
    private val connectedDevices = ConcurrentHashMap<String, BluetoothDevice>()

    // Per-device subscription set: address → which TX char UUIDs the peer has
    // enabled notifications for (Ratspeak TX, Columba TX, or both). Populated
    // when the peer writes the CCCD descriptor with [0x01, 0x00].
    private val connectedCentrals = ConcurrentHashMap<String, MutableSet<UUID>>()

    // TX characteristics indexed by UUID, populated at openGattServer time.
    // notifyTx looks up the characteristic to call notifyCharacteristicChanged on.
    private val txCharacteristics = ConcurrentHashMap<UUID, BluetoothGattCharacteristic>()

    // Per-central negotiated ATT payload size (MTU - 3). Populated by
    // RatspeakGattCallback.onMtuChanged. Defaults to 244 (BLE 4.2+ baseline)
    // if unknown so notifyTx can size fragments without waiting for the
    // exchange (B6).
    private val centralMtu = ConcurrentHashMap<String, Int>()
    private const val DEFAULT_PAYLOAD = 244

    // Per-device notify gate. Android accepts only one outstanding
    // notification per device and confirms delivery via onNotificationSent;
    // firing the next notify before then silently drops it, corrupting a
    // multi-fragment packet. Each device holds a 1-permit semaphore: notifyTx
    // waits for the previous send's onNotificationSent (bounded, so a lost
    // callback can't wedge the fan-out) before issuing the next.
    private val notifyGate = ConcurrentHashMap<String, Semaphore>()
    private const val NOTIFY_GATE_TIMEOUT_MS = 250L

    private fun notifyGate(deviceAddress: String): Semaphore =
        notifyGate.getOrPut(deviceAddress) { Semaphore(1, true) }

    /**
     * Open the GATT server and register both Ratspeak and Columba services.
     * Returns true on success.
     */
    @JvmStatic
    // identityHash is retained in the JNI signature for ABI stability but is no
    // longer surfaced over GATT (the static ID characteristic was removed as a
    // tracking vector); identity is exchanged via signed announces.
    @Suppress("UNUSED_PARAMETER")
    fun openGattServer(context: Context, identityHash: ByteArray): Boolean {
        val btManager = context.getSystemService(Context.BLUETOOTH_SERVICE) as? BluetoothManager
        if (btManager == null) {
            Log.e(TAG, "GATT server: BluetoothManager not available")
            return false
        }

        val callback = RatspeakGattCallback({ gattServer })
        gattServer = btManager.openGattServer(context, callback)
        if (gattServer == null) {
            Log.e(TAG, "GATT server: openGattServer returned null")
            return false
        }

        // Register Ratspeak service
        val ratspeakService = createService(
            RATSPEAK_SERVICE, RATSPEAK_RX, RATSPEAK_TX
        )
        if (!gattServer!!.addService(ratspeakService)) {
            Log.w(TAG, "GATT server: failed to add Ratspeak service")
        }

        // Small delay for Android service queue, then add Columba
        Thread.sleep(250)

        val columbaService = createService(
            COLUMBA_SERVICE, COLUMBA_RX, COLUMBA_TX
        )
        if (!gattServer!!.addService(columbaService)) {
            Log.w(TAG, "GATT server: failed to add Columba service")
        }

        Log.i(TAG, "GATT server: opened with Ratspeak + Columba services")
        return true
    }

    /**
     * Close the GATT server and release resources.
     */
    @JvmStatic
    fun closeGattServer() {
        gattServer?.close()
        gattServer = null
        connectedDevices.clear()
        connectedCentrals.clear()
        centralMtu.clear()
        txCharacteristics.clear()
        Log.i(TAG, "GATT server: closed")
    }

    private fun createService(
        serviceUuid: UUID, rxUuid: UUID, txUuid: UUID
    ): BluetoothGattService {
        val service = BluetoothGattService(
            serviceUuid, BluetoothGattService.SERVICE_TYPE_PRIMARY
        )

        // RX: remote peers write data to us
        val rx = BluetoothGattCharacteristic(
            rxUuid,
            BluetoothGattCharacteristic.PROPERTY_WRITE or
                BluetoothGattCharacteristic.PROPERTY_WRITE_NO_RESPONSE,
            BluetoothGattCharacteristic.PERMISSION_WRITE
        )
        service.addCharacteristic(rx)

        // TX: we send notifications to connected peers
        val tx = BluetoothGattCharacteristic(
            txUuid,
            BluetoothGattCharacteristic.PROPERTY_READ or
                BluetoothGattCharacteristic.PROPERTY_NOTIFY,
            BluetoothGattCharacteristic.PERMISSION_READ
        )
        // CCCD descriptor required for the peer to enable notifications
        val cccd = BluetoothGattDescriptor(
            CCCD_UUID,
            BluetoothGattDescriptor.PERMISSION_READ or
                BluetoothGattDescriptor.PERMISSION_WRITE
        )
        tx.addDescriptor(cccd)
        service.addCharacteristic(tx)
        // Stash the TX characteristic so notifyTx can resolve UUID → live char
        txCharacteristics[txUuid] = tx

        // No static ID characteristic: it exposed a MAC-rotation-stable
        // identity read to any connecting scanner (a tracking vector) and was
        // never read by this stack — identity is learned from signed announces.

        return service
    }

    // ── Connection tracking (called from RatspeakGattCallback) ───────────────

    /** Record a newly connected central so notifyTx can address it later. */
    @JvmStatic
    fun recordConnection(device: BluetoothDevice) {
        val addr = device.address ?: return
        connectedDevices[addr] = device
    }

    /** Drop a disconnected central and any subscription it had. */
    @JvmStatic
    fun removeConnection(deviceAddress: String) {
        connectedDevices.remove(deviceAddress)
        connectedCentrals.remove(deviceAddress)
        centralMtu.remove(deviceAddress)
        notifyGate.remove(deviceAddress)
    }

    /** Stash the negotiated ATT payload size for a connected central (B6). */
    @JvmStatic
    fun recordMtu(deviceAddress: String, payloadSize: Int) {
        if (payloadSize > 0) {
            centralMtu[deviceAddress] = payloadSize
        }
    }

    /** Negotiated payload size for `deviceAddress`, or the safe default. */
    @JvmStatic
    fun getMtu(deviceAddress: String): Int {
        return centralMtu[deviceAddress] ?: DEFAULT_PAYLOAD
    }

    /**
     * Min negotiated payload size across every central currently subscribed
     * to `characteristicUuidStr`. The broadcast fan-out fragments at this
     * size so a single notify reaches everyone without further chunking.
     * Returns [DEFAULT_PAYLOAD] when there are no subscribers.
     */
    @JvmStatic
    fun getMinSubscribedPayload(characteristicUuidStr: String): Int {
        val charUuid = try { UUID.fromString(characteristicUuidStr) } catch (_: Throwable) { return DEFAULT_PAYLOAD }
        var min = Int.MAX_VALUE
        for ((addr, subs) in connectedCentrals) {
            if (!subs.contains(charUuid)) continue
            val m = centralMtu[addr] ?: DEFAULT_PAYLOAD
            if (m < min) min = m
        }
        return if (min == Int.MAX_VALUE) DEFAULT_PAYLOAD else min
    }

    /** Mark this central as having enabled notifications on a TX characteristic. */
    @JvmStatic
    fun recordSubscription(deviceAddress: String, characteristicUuidStr: String) {
        val charUuid = try { UUID.fromString(characteristicUuidStr) } catch (_: Throwable) { return }
        val set = connectedCentrals.getOrPut(deviceAddress) { java.util.concurrent.ConcurrentHashMap.newKeySet() }
        set.add(charUuid)
        Log.i(TAG, "GATT: $deviceAddress subscribed to $charUuid")
    }

    /** Mark this central as having disabled notifications on a TX characteristic. */
    @JvmStatic
    fun removeSubscription(deviceAddress: String, characteristicUuidStr: String) {
        val charUuid = try { UUID.fromString(characteristicUuidStr) } catch (_: Throwable) { return }
        connectedCentrals[deviceAddress]?.remove(charUuid)
        Log.i(TAG, "GATT: $deviceAddress unsubscribed from $charUuid")
    }

    // ── Outbound notify path (called from Rust via JNI) ──────────────────────

    /**
     * Push `data` to one specific subscribed central as a NOTIFY on the named
     * TX characteristic. Returns true if the notification was queued.
     *
     * Uses the API 33 value-carrying notify overload where available and keeps
     * the older setValue/notify pair for API 24-32 compatibility.
     */
    @JvmStatic
    fun notifyTx(deviceAddress: String, characteristicUuidStr: String, data: ByteArray): Boolean {
        val server = gattServer ?: return false
        val device = connectedDevices[deviceAddress] ?: return false
        val charUuid = try { UUID.fromString(characteristicUuidStr) } catch (_: Throwable) { return false }
        val char = txCharacteristics[charUuid] ?: return false

        // Skip if this central has not enabled notifications (avoids the
        // BluetoothGattServer error log spam Android emits in that case).
        if (connectedCentrals[deviceAddress]?.contains(charUuid) != true) {
            return false
        }

        // Wait for the previous notification to be taken by the stack. Bounded
        // so a missing onNotificationSent degrades to best-effort rather than
        // wedging the fan-out. On a successful enqueue the gate is released by
        // onNotifySent; on failure we release it here since no callback comes.
        val gate = notifyGate(deviceAddress)
        val acquired = gate.tryAcquire(NOTIFY_GATE_TIMEOUT_MS, TimeUnit.MILLISECONDS)

        val ok = try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                server.notifyCharacteristicChanged(device, char, false, data) == BluetoothStatusCodes.SUCCESS
            } else {
                setCharacteristicValueCompat(char, data)
                @Suppress("DEPRECATION")
                server.notifyCharacteristicChanged(device, char, false)
            }
        } catch (t: Throwable) {
            Log.w(TAG, "notifyTx failed for $deviceAddress: ${t.message}")
            false
        }

        if (!ok && acquired) {
            gate.release()
        }
        return ok
    }

    /**
     * Called from RatspeakGattCallback.onNotificationSent to release the
     * per-device notify gate so the next fragment can be sent. Capped at one
     * permit so a duplicate/spurious callback can't over-release.
     */
    @JvmStatic
    fun onNotifySent(deviceAddress: String) {
        val gate = notifyGate[deviceAddress] ?: return
        if (gate.availablePermits() == 0) {
            gate.release()
        }
    }

    /**
     * Broadcast `data` to every subscribed central on the named TX characteristic,
     * optionally excluding `excludeAddress` (used for B2 anti-loop fan-out so
     * the originator of an inbound packet does not receive its own packet back).
     *
     * Returns the count of subscribers we attempted to notify.
     */
    @JvmStatic
    fun broadcastTx(characteristicUuidStr: String, data: ByteArray, excludeAddress: String?): Int {
        val charUuid = try { UUID.fromString(characteristicUuidStr) } catch (_: Throwable) { return 0 }
        var sent = 0
        for ((addr, subs) in connectedCentrals) {
            if (addr == excludeAddress) continue
            if (!subs.contains(charUuid)) continue
            if (notifyTx(addr, characteristicUuidStr, data)) {
                sent++
            }
        }
        return sent
    }

    /**
     * Snapshot of central addresses currently subscribed to the named TX
     * characteristic. The Rust fan-out enumerates this list and filters per
     * peer through the in-process anti-loop map (B2) before issuing
     * individual `notifyTx` calls — broadcastTx only supports a single
     * exclusion, but B2 may have many sources to skip per outbound packet.
     *
     * Returns a newline-separated string (empty when no subscribers) so the
     * JNI layer can move it across as a single jstring without juggling a
     * Java array reference.
     */
    @JvmStatic
    fun subscribedAddressesFor(characteristicUuidStr: String): String {
        val charUuid = try { UUID.fromString(characteristicUuidStr) } catch (_: Throwable) { return "" }
        val sb = StringBuilder()
        for ((addr, subs) in connectedCentrals) {
            if (!subs.contains(charUuid)) continue
            if (sb.isNotEmpty()) sb.append('\n')
            sb.append(addr)
        }
        return sb.toString()
    }

    /**
     * Snapshot every remote central currently attached to our local GATT server,
     * including devices that have connected but not yet completed CCCD
     * subscription. Rust uses this to avoid initiating an opposite-direction
     * GATT client connection to the same phone, which triggers flaky Android
     * dual-role collision behavior on some devices.
     */
    @JvmStatic
    fun connectedOrSubscribedAddresses(): String {
        val addresses = linkedSetOf<String>()
        addresses.addAll(connectedDevices.keys)
        addresses.addAll(connectedCentrals.keys)
        return addresses.joinToString("\n")
    }

    @Suppress("DEPRECATION")
    private fun setCharacteristicValueCompat(
        characteristic: BluetoothGattCharacteristic,
        value: ByteArray
    ) {
        characteristic.value = value
    }
}
