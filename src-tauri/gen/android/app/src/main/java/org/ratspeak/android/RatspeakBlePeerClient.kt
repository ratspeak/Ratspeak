package org.ratspeak.android

import android.annotation.SuppressLint
import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothGatt
import android.bluetooth.BluetoothGattCallback
import android.bluetooth.BluetoothGattCharacteristic
import android.bluetooth.BluetoothGattDescriptor
import android.bluetooth.BluetoothGattService
import android.bluetooth.BluetoothManager
import android.bluetooth.BluetoothProfile
import android.bluetooth.BluetoothStatusCodes
import android.bluetooth.le.ScanCallback
import android.bluetooth.le.ScanFilter
import android.bluetooth.le.ScanResult
import android.bluetooth.le.ScanSettings
import android.content.Context
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.ParcelUuid
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Native BLE Central / GATT client for Ratspeak's symmetric BLE mesh.
 *
 * Mirrors [RatspeakBleGatt] but for **peer-to-peer mesh connections**, not
 * RNode. Differences:
 *  - **No bonding / SMP** — Reticulum's link layer + LXMF provide end-to-end
 *    encryption, so the BLE link runs unencrypted by design (Bitchat-style).
 *  - **No TCP bridge** — inbound bytes are pushed straight into Rust via the
 *    [nativePeerClientDataReceived] JNI callback; outbound writes come back
 *    in via the static [write] dispatcher.
 *  - **Dual-service discovery** — a peer may expose either the Ratspeak
 *    service (preferred) or the Columba compatibility service. We try
 *    Ratspeak first.
 *
 * Why bypass btleplug here when btleplug already implements all of this on
 * desktop platforms? On Android 14+ btleplug's Java layer uses the
 * deprecated `BluetoothAdapter.getDefaultAdapter()` + `connectGatt(null, ...)`
 * which fail at runtime. The same workaround pattern exists for [RatspeakBleGatt]
 * (RNode); this class extends it to peer connections.
 */
@SuppressLint("MissingPermission")
class RatspeakBlePeerClient(private val context: Context) {

    companion object {
        private const val TAG = "RatspeakBlePeer"

        // BLE 5.0 max ATT MTU. The peer is free to negotiate down; we keep a
        // safe fallback so writes never silently truncate.
        private const val TARGET_MTU = 517
        private const val MTU_FALLBACK_PAYLOAD = 244
        private const val GATT_TIMEOUT_SEC = 15L

        // Service + characteristic UUIDs — must match ble_peer.rs and
        // RatspeakBleServer.kt exactly.
        val RATSPEAK_SERVICE: UUID = UUID.fromString("a1b2c3d4-e5f6-4a5b-8c9d-0e1f2a3b4c5d")
        val RATSPEAK_RX: UUID      = UUID.fromString("a1b2c3d4-e5f6-4a5b-8c9d-0e1f2a3b4c5e")
        val RATSPEAK_TX: UUID      = UUID.fromString("a1b2c3d4-e5f6-4a5b-8c9d-0e1f2a3b4c5f")
        val RATSPEAK_ID: UUID      = UUID.fromString("a1b2c3d4-e5f6-4a5b-8c9d-0e1f2a3b4c60")

        val COLUMBA_SERVICE: UUID = UUID.fromString("37145b00-442d-4a94-917f-8f42c5da28e3")
        val COLUMBA_RX: UUID      = UUID.fromString("37145b00-442d-4a94-917f-8f42c5da28e5")
        val COLUMBA_TX: UUID      = UUID.fromString("37145b00-442d-4a94-917f-8f42c5da28e4")
        val COLUMBA_ID: UUID      = UUID.fromString("37145b00-442d-4a94-917f-8f42c5da28e6")

        val CCCD: UUID = UUID.fromString("00002902-0000-1000-8000-00805f9b34fb")

        // Per-address registry so Rust JNI can resolve a string address back
        // to the live client instance. The peer interface owns connection
        // lifecycle, so each address has at most one active client.
        private val clients = ConcurrentHashMap<String, RatspeakBlePeerClient>()

        /**
         * Static write dispatcher called from Rust JNI. Forwards `data` to the
         * RX characteristic of the client matching `address`. Returns true if
         * the write was queued; false if the address has no live client.
         */
        @JvmStatic
        fun write(address: String, data: ByteArray): Boolean {
            return clients[address]?.writeRx(data) ?: false
        }

        /**
         * Static disconnect dispatcher (Rust JNI). Tears down the client for
         * `address` if one exists. Idempotent.
         */
        @JvmStatic
        fun disconnect(address: String) {
            clients.remove(address)?.cleanup()
        }

        /**
         * Negotiated ATT payload size (MTU - 3) for the central connection
         * to `address`, or [MTU_FALLBACK_PAYLOAD] if unknown / not connected.
         * Used by the Rust fan-out loop to size outbound fragments per peer.
         */
        @JvmStatic
        fun getMtu(address: String): Int {
            return clients[address]?.negotiatedMtu ?: MTU_FALLBACK_PAYLOAD
        }

        /**
         * Static connect dispatcher (Rust JNI). Constructs a fresh client,
         * runs the connect+subscribe sequence, and returns true on success.
         * The client registers itself in [clients] during a successful
         * [connect], so subsequent [write]/[disconnect] dispatches resolve
         * correctly.
         *
         * Bitchat-style: no BLE-level identity exchange — identity flows in
         * later via the first signed Reticulum announce.
         *
         * Caller must invoke this off the main thread (the connect sequence
         * uses CountDownLatches that would deadlock on the GATT callback
         * thread otherwise). Rust does this via `tokio::task::spawn_blocking`.
         */
        @JvmStatic
        fun connectFromNative(context: Context, address: String): Boolean {
            val existing = clients[address]
            if (existing != null && existing.isRunning()) {
                Log.w(TAG, "connectFromNative: already connected to $address")
                return false
            }
            val client = RatspeakBlePeerClient(context)
            return client.connect(address)
        }

        /**
         * Static scan dispatcher (Rust JNI). Scans for peers advertising
         * either the Ratspeak or Columba mesh service for `timeoutMs`,
         * returning a list of `address|rssi|protocol` strings (pipe-delimited)
         * where protocol is "ratspeak" or "columba". Returning a flat string
         * list is the cheapest JNI shape — Rust splits client-side.
         *
         * Uses an OS-side ScanFilter so the radio firmware filters by service
         * UUID, which is dramatically more battery-efficient than software
         * filtering on every advertisement.
         */
        @JvmStatic
        fun scanMesh(context: Context, timeoutMs: Long): Array<String> {
            val bm = context.getSystemService(BluetoothManager::class.java)
                ?: return emptyArray()
            val adapter = bm.adapter ?: return emptyArray()
            if (!adapter.isEnabled) return emptyArray()
            val scanner = adapter.bluetoothLeScanner ?: return emptyArray()

            val filters = listOf(
                ScanFilter.Builder().setServiceUuid(ParcelUuid(RATSPEAK_SERVICE)).build(),
                ScanFilter.Builder().setServiceUuid(ParcelUuid(COLUMBA_SERVICE)).build(),
            )
            val settings = ScanSettings.Builder()
                .setScanMode(ScanSettings.SCAN_MODE_LOW_LATENCY)
                .build()

            // Per-address best-known result so two adverts from the same peer
            // (one Ratspeak, one Columba) resolve to a single row, with
            // Ratspeak winning the protocol pick.
            data class Hit(var rssi: Int, var ratspeak: Boolean)
            val hits = ConcurrentHashMap<String, Hit>()
            val callback = object : ScanCallback() {
                override fun onScanResult(callbackType: Int, result: ScanResult) {
                    val addr = result.device?.address ?: return
                    val rssi = result.rssi
                    val uuids = result.scanRecord?.serviceUuids?.map { it.uuid } ?: emptyList()
                    val isRatspeak = uuids.contains(RATSPEAK_SERVICE)
                    val isColumba = uuids.contains(COLUMBA_SERVICE)
                    if (!isRatspeak && !isColumba) return
                    val existing = hits[addr]
                    if (existing == null) {
                        hits[addr] = Hit(rssi, isRatspeak)
                    } else {
                        existing.rssi = rssi
                        if (isRatspeak) existing.ratspeak = true
                    }
                }
            }

            try {
                scanner.startScan(filters, settings, callback)
                Thread.sleep(timeoutMs.coerceAtLeast(100))
            } catch (t: Throwable) {
                Log.w(TAG, "scanMesh failed: ${t.message}")
            } finally {
                try { scanner.stopScan(callback) } catch (_: Throwable) {}
            }

            return hits.entries.map { (addr, hit) ->
                "$addr|${hit.rssi}|${if (hit.ratspeak) "ratspeak" else "columba"}"
            }.toTypedArray()
        }
    }

    /**
     * Which service variant the connected peer exposes. Set during
     * [discoverAndPickService]; determines which RX/TX UUIDs we use.
     */
    enum class PeerProtocol { RATSPEAK, COLUMBA }

    private var gatt: BluetoothGatt? = null
    private var rxChar: BluetoothGattCharacteristic? = null
    private var txChar: BluetoothGattCharacteristic? = null
    @Volatile internal var negotiatedMtu = MTU_FALLBACK_PAYLOAD
    private var protocol: PeerProtocol = PeerProtocol.RATSPEAK
    private var address: String = ""
    private val running = AtomicBoolean(false)

    private var connectLatch: CountDownLatch? = null
    private var servicesLatch: CountDownLatch? = null
    private var descriptorLatch: CountDownLatch? = null
    private var mtuLatch: CountDownLatch? = null

    private val connectStatus = AtomicBoolean(false)
    private val servicesStatus = AtomicBoolean(false)

    private val handler = Handler(Looper.getMainLooper())

    /**
     * Connect to a peer at `address`. On success, subscribes to TX
     * notifications and returns true. On failure, returns false and emits
     * an error log line; cleanup() is run automatically.
     *
     * Bitchat-style: no BLE-level identity exchange. Identity is learned
     * later from the first signed Reticulum announce that flows over the
     * link — the GATT-level ID characteristic is no longer read here.
     */
    fun connect(address: String): Boolean {
        this.address = address
        try {
            Log.i(TAG, "=== PEER CONNECT START === $address")

            val bm = context.getSystemService(BluetoothManager::class.java)
                ?: return errFalse("BluetoothManager unavailable")
            val adapter = bm.adapter
                ?: return errFalse("No Bluetooth adapter")
            if (!adapter.isEnabled) return errFalse("Bluetooth disabled")

            val device: BluetoothDevice = try { adapter.getRemoteDevice(address) }
            catch (e: Exception) { return errFalse("Invalid address $address: ${e.message}") }

            // ── Phase 1: GATT connect (no bonding, no auto-connect) ──
            connectLatch = CountDownLatch(1)
            connectStatus.set(false)
            handler.post {
                gatt = device.connectGatt(context, false, gattCallback, BluetoothDevice.TRANSPORT_LE)
            }
            if (!connectLatch!!.await(GATT_TIMEOUT_SEC, TimeUnit.SECONDS) || !connectStatus.get()) {
                cleanup(); return errFalse("GATT connect timeout/failure")
            }

            // Register self under the device address so Rust outbound writes
            // can find this client. Done after a successful connect so a failed
            // connection never leaves a stale entry behind.
            clients[address] = this

            // ── Phase 2: MTU negotiation (best-effort) ──
            mtuLatch = CountDownLatch(1)
            handler.post { gatt?.requestMtu(TARGET_MTU) }
            mtuLatch!!.await(5, TimeUnit.SECONDS)

            // ── Phase 3: Service discovery ──
            servicesLatch = CountDownLatch(1)
            servicesStatus.set(false)
            handler.post { gatt?.discoverServices() }
            if (!servicesLatch!!.await(15, TimeUnit.SECONDS) || !servicesStatus.get()) {
                cleanup(); return errFalse("Service discovery failed")
            }

            // ── Phase 4: Pick Ratspeak or Columba ──
            if (!discoverAndPickService()) {
                cleanup(); return errFalse("Neither Ratspeak nor Columba service found")
            }

            // ── Phase 5: Subscribe to TX notifications ──
            descriptorLatch = CountDownLatch(1)
            val tx = txChar ?: run { cleanup(); return errFalse("TX characteristic missing") }
            gatt?.setCharacteristicNotification(tx, true)
            tx.getDescriptor(CCCD)?.let { desc ->
                gatt?.let { activeGatt ->
                    writeDescriptorCompat(
                        activeGatt,
                        desc,
                        BluetoothGattDescriptor.ENABLE_NOTIFICATION_VALUE
                    )
                }
            } ?: run { cleanup(); return errFalse("CCCD descriptor missing") }
            descriptorLatch!!.await(10, TimeUnit.SECONDS)

            running.set(true)
            Log.i(TAG, "=== PEER CONNECT SUCCESS === protocol=$protocol mtu=$negotiatedMtu")
            return true

        } catch (e: Exception) {
            cleanup()
            return errFalse("Exception: ${e.javaClass.simpleName}: ${e.message}")
        }
    }

    private fun discoverAndPickService(): Boolean {
        val g = gatt ?: return false
        val ratspeak = g.getService(RATSPEAK_SERVICE)
        if (ratspeak != null) {
            rxChar = ratspeak.getCharacteristic(RATSPEAK_RX)
            txChar = ratspeak.getCharacteristic(RATSPEAK_TX)
            if (rxChar != null && txChar != null) {
                protocol = PeerProtocol.RATSPEAK
                return true
            }
        }
        val columba = g.getService(COLUMBA_SERVICE)
        if (columba != null) {
            rxChar = columba.getCharacteristic(COLUMBA_RX)
            txChar = columba.getCharacteristic(COLUMBA_TX)
            if (rxChar != null && txChar != null) {
                protocol = PeerProtocol.COLUMBA
                return true
            }
        }
        return false
    }

    /**
     * Write `data` to the peer's RX characteristic. Caller is responsible for
     * fragmentation — we forward the bytes verbatim. Returns true on
     * successful enqueue.
     */
    fun writeRx(data: ByteArray): Boolean {
        if (!running.get()) return false
        val rx = rxChar ?: return false
        val g = gatt ?: return false
        return try {
            // Use no-response writes for throughput; the protocol layer above
            // (LXMF / Reticulum) handles delivery acknowledgement.
            writeCharacteristicCompat(
                g,
                rx,
                data,
                BluetoothGattCharacteristic.WRITE_TYPE_NO_RESPONSE
            )
        } catch (t: Throwable) {
            Log.w(TAG, "writeRx failed: ${t.message}")
            false
        }
    }

    /** Disconnect and free GATT resources. Idempotent. */
    fun cleanup() {
        Log.i(TAG, "cleanup() $address")
        running.set(false)
        if (address.isNotEmpty()) clients.remove(address, this)
        handler.post {
            try { txChar?.let { gatt?.setCharacteristicNotification(it, false) } } catch (_: Exception) {}
            try { gatt?.disconnect() } catch (_: Exception) {}
            try { gatt?.close() } catch (_: Exception) {}
            gatt = null
        }
        rxChar = null; txChar = null
    }

    fun isRunning(): Boolean = running.get()

    private fun errFalse(msg: String): Boolean { Log.e(TAG, msg); return false }

    @SuppressLint("MissingPermission")
    private fun writeDescriptorCompat(
        activeGatt: BluetoothGatt,
        descriptor: BluetoothGattDescriptor,
        value: ByteArray
    ): Boolean {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            return activeGatt.writeDescriptor(descriptor, value) == BluetoothStatusCodes.SUCCESS
        }

        @Suppress("DEPRECATION")
        descriptor.value = value
        @Suppress("DEPRECATION")
        return activeGatt.writeDescriptor(descriptor)
    }

    @SuppressLint("MissingPermission")
    private fun writeCharacteristicCompat(
        activeGatt: BluetoothGatt,
        characteristic: BluetoothGattCharacteristic,
        value: ByteArray,
        writeType: Int
    ): Boolean {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            return activeGatt.writeCharacteristic(characteristic, value, writeType) == BluetoothStatusCodes.SUCCESS
        }

        characteristic.writeType = writeType
        @Suppress("DEPRECATION")
        characteristic.value = value
        @Suppress("DEPRECATION")
        return activeGatt.writeCharacteristic(characteristic)
    }

    private fun handleCharacteristicChanged(ch: BluetoothGattCharacteristic, data: ByteArray) {
        if (ch.uuid == RATSPEAK_TX || ch.uuid == COLUMBA_TX) {
            // Push straight through to Rust; the per-peer reassembler there
            // will tag by address and recombine fragments.
            nativePeerClientDataReceived(address, data)
        }
    }

    private val gattCallback = object : BluetoothGattCallback() {
        override fun onConnectionStateChange(g: BluetoothGatt, status: Int, newState: Int) {
            Log.i(TAG, "GATT $address: status=$status state=$newState")
            when (newState) {
                BluetoothProfile.STATE_CONNECTED -> {
                    connectStatus.set(true)
                    connectLatch?.countDown()
                }
                BluetoothProfile.STATE_DISCONNECTED -> {
                    connectStatus.set(false)
                    connectLatch?.countDown()
                    if (running.getAndSet(false)) {
                        // Notify Rust the peer dropped so the central mesh-state
                        // can prune it without waiting for a keepalive miss.
                        nativePeerClientDisconnected(address)
                    }
                }
            }
        }

        override fun onMtuChanged(g: BluetoothGatt, mtu: Int, status: Int) {
            if (status == BluetoothGatt.GATT_SUCCESS) negotiatedMtu = mtu - 3
            mtuLatch?.countDown()
        }

        override fun onServicesDiscovered(g: BluetoothGatt, status: Int) {
            servicesStatus.set(status == BluetoothGatt.GATT_SUCCESS)
            servicesLatch?.countDown()
        }

        override fun onDescriptorWrite(g: BluetoothGatt, desc: BluetoothGattDescriptor, status: Int) {
            descriptorLatch?.countDown()
        }

        override fun onCharacteristicChanged(
            g: BluetoothGatt,
            ch: BluetoothGattCharacteristic,
            value: ByteArray
        ) {
            handleCharacteristicChanged(ch, value)
        }

        @Suppress("DEPRECATION", "OVERRIDE_DEPRECATION")
        override fun onCharacteristicChanged(g: BluetoothGatt, ch: BluetoothGattCharacteristic) {
            handleCharacteristicChanged(ch, ch.value ?: return)
        }
    }

    // Native methods registered by Rust in JNI_OnLoad. The Rust extern fns
    // live in ble_peer.rs::android_peripheral.
    private external fun nativePeerClientDataReceived(address: String, data: ByteArray)
    private external fun nativePeerClientDisconnected(address: String)
}
