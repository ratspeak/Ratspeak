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
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.webkit.WebView
import java.io.InputStream
import java.io.OutputStream
import java.net.ServerSocket
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Native BLE GATT client + local TCP bridge for RNode communication.
 *
 * RNode requires MITM-encrypted BLE connections (NUS over bonded link).
 * Connection sequence:
 *   1. createBond() first when Android reports the device is unpaired
 *   2. Wait for bonding — Android shows pairing dialog, user confirms
 *   3. After BOND_BONDED, let the RNode finish its intentional post-pair disconnect
 *   4. connectGatt() over the bonded link, where encryption is automatic
 *   5. Service discovery → NUS characteristics → TCP bridge
 */
class RatspeakBleGatt(private val context: Context) {

    companion object {
        private const val TAG = "RatspeakBleGatt"

        private const val TARGET_MTU = 515
        // Safe fallback payload if MTU negotiation never completes (BLE 4.2 floor
        // minus the 3-byte ATT header). 20-byte default clogs the pipe.
        private const val MTU_FALLBACK_PAYLOAD = 244
        private const val GATT_TIMEOUT_SEC = 15L
        // Cardputer RNode keeps first-pair/manual pairing windows open longer
        // than this; time out first so the app can cleanly roll back.
        private const val BOND_TIMEOUT_SEC = 60L
        private const val BOND_POLL_INTERVAL_MS = 250L
        private const val POST_BOND_RECONNECT_DELAY_MS = 2600L

        // TCP read buffer. Large because one write from Rust can be up to 4KB;
        // the per-chunk BLE write uses negotiatedMtu separately.
        private const val TCP_READ_BUFFER = 4096

        // Upstream RNodeInterface.detach(): RADIO_STATE_OFF, then LEAVE.
        private val RNODE_DETACH_FRAME = byteArrayOf(
            0xC0.toByte(), 0x06, 0x00, 0xC0.toByte(),
            0xC0.toByte(), 0x0A, 0xFF.toByte(), 0xC0.toByte(),
        )

        // Error prefixes the JS side recognises to drive UX.
        const val ERR_PAIRING_MODE = "ERR_PAIRING_MODE"
    }

    private var gatt: BluetoothGatt? = null
    private var rxChar: BluetoothGattCharacteristic? = null
    private var txChar: BluetoothGattCharacteristic? = null
    private var negotiatedMtu = MTU_FALLBACK_PAYLOAD
    private val running = AtomicBoolean(false)

    private var serverSocket: ServerSocket? = null
    private var clientSocket: java.net.Socket? = null
    private var tcpOut: OutputStream? = null
    private var forwardThread: Thread? = null

    private var connectLatch: CountDownLatch? = null
    private var servicesLatch: CountDownLatch? = null
    private var descriptorLatch: CountDownLatch? = null
    private var mtuLatch: CountDownLatch? = null
    private var bondLatch: CountDownLatch? = null

    // Volatile/atomic because GATT callbacks fire off the main thread.
    private val connectStatus = AtomicBoolean(false)
    private val servicesStatus = AtomicBoolean(false)
    private var bondReceiver: BroadcastReceiver? = null
    private var webViewRef: WebView? = null

    private val handler = Handler(Looper.getMainLooper())

    /** Register the WebView so connect phases can push progress updates to JS. */
    fun attachWebView(webView: WebView?) { webViewRef = webView }

    @SuppressLint("MissingPermission")
    fun connect(address: String, localPort: Int): String? {
        try {
            Log.i(TAG, "=== BLE CONNECT START === address=$address tcpPort=$localPort")
            emitProgress("starting")

            val bluetoothManager = context.getSystemService(BluetoothManager::class.java)
                ?: return err("Bluetooth service not available")
            val adapter = bluetoothManager.adapter
                ?: return err("No Bluetooth adapter found")
            if (!adapter.isEnabled)
                return err("Bluetooth is turned off")

            val device: BluetoothDevice = try {
                adapter.getRemoteDevice(address)
            } catch (e: Exception) {
                return err("Invalid BLE address: $address")
            }

            Log.i(TAG, "Device: name=${device.name} bondState=${bondStr(device.bondState)}")

            // ── Phase 0: Bond first (if needed) ──
            // RNode requires MITM-encrypted BLE (ESP_LE_AUTH_REQ_SC_MITM_BOND), so
            // we must own the SMP exchange explicitly via createBond() rather than
            // letting connectGatt() trigger it implicitly. The implicit path on
            // Android pops a "Pair & Connect" dialog *and then* a passkey dialog;
            // the explicit path pops one passkey dialog. Do this before any GATT
            // operations so the link is encrypted by the time we discover services.
            //
            // RNode pairing mode (bt_allow_pairing=true) must already be active —
            // the modal's pre-bond step makes that the user's job before they
            // even open this code path.
            var bondedDuringThisConnect = false
            if (device.bondState != BluetoothDevice.BOND_BONDED) {
                emitProgress("bonding")
                bondLatch = CountDownLatch(1)
                registerBondReceiver(address)
                Log.i(TAG, "Phase 0: createBond() (RNode must be in pairing mode, up to ${BOND_TIMEOUT_SEC}s)...")
                val started = try { device.createBond() } catch (e: Exception) {
                    logEx("createBond", e); false
                }
                if (!started && device.bondState != BluetoothDevice.BOND_BONDING) {
                    unregisterBondReceiver()
                    return err("$ERR_PAIRING_MODE createBond() rejected — Bluetooth state may be unstable")
                }
                if (!waitForBondState(device) && device.bondState != BluetoothDevice.BOND_BONDED) {
                    unregisterBondReceiver()
                    return err("$ERR_PAIRING_MODE Bonding timed out (${bondStr(device.bondState)})")
                }
                if (device.bondState != BluetoothDevice.BOND_BONDED) {
                    unregisterBondReceiver()
                    return err("$ERR_PAIRING_MODE Bonding failed (${bondStr(device.bondState)})")
                }
                Log.i(TAG, "Phase 0 COMPLETE: Bonded")
                bondedDuringThisConnect = true
            }
            unregisterBondReceiver()
            if (bondedDuringThisConnect) {
                // rsCardputer/RNode intentionally disconnects shortly after
                // pairing succeeds so the host reconnects over the stored bond.
                // Starting GATT during that window makes the first detect/init
                // attempt race a deliberate peripheral-side disconnect.
                emitProgress("pairing_settle")
                Log.i(TAG, "Phase 0b: waiting ${POST_BOND_RECONNECT_DELAY_MS}ms for RNode post-pair disconnect")
                Thread.sleep(POST_BOND_RECONNECT_DELAY_MS)
            }

            // ── Phase 1: GATT connect over the bonded (encrypted) link ──
            connectLatch = CountDownLatch(1)
            connectStatus.set(false)

            emitProgress("connecting")
            Log.i(TAG, "Phase 1: Connecting GATT...")
            handler.post {
                gatt = device.connectGatt(context, false, gattCallback, BluetoothDevice.TRANSPORT_LE)
                Log.i(TAG, "connectGatt() called")
            }

            if (!connectLatch!!.await(GATT_TIMEOUT_SEC, TimeUnit.SECONDS)) {
                cleanup(); return err("GATT connection timed out")
            }
            if (!connectStatus.get()) {
                cleanup(); return err("GATT connection failed")
            }
            Log.i(TAG, "Phase 1 COMPLETE: GATT connected")

            // ── Phase 2: MTU negotiation ──
            emitProgress("mtu")
            Log.i(TAG, "Phase 2: Requesting MTU=$TARGET_MTU")
            mtuLatch = CountDownLatch(1)
            handler.post { gatt?.requestMtu(TARGET_MTU) }
            if (!mtuLatch!!.await(5, TimeUnit.SECONDS)) {
                // Fall back to 244 rather than sticking at 20 (initial BLE default).
                negotiatedMtu = MTU_FALLBACK_PAYLOAD
                Log.w(TAG, "MTU negotiation timed out — using fallback payload=$MTU_FALLBACK_PAYLOAD")
            }
            Log.i(TAG, "Phase 2 COMPLETE: MTU payload=$negotiatedMtu")

            // ── Phase 3: Service discovery ──
            emitProgress("discovering")
            Log.i(TAG, "Phase 3: Discovering services...")
            if (!discoverServicesWithLatch(15)) {
                cleanup(); return err("Service discovery failed")
            }

            gatt?.services?.forEach { svc ->
                Log.i(TAG, "  Service: ${svc.uuid}")
                svc.characteristics.forEach { c ->
                    Log.i(TAG, "    Char: ${c.uuid} props=0x${c.properties.toString(16)}")
                }
            }

            val nusService: BluetoothGattService = gatt?.getService(BleUuids.NUS_SERVICE)
                ?: run { cleanup(); return err("NUS service not found") }
            rxChar = nusService.getCharacteristic(BleUuids.NUS_RX_CHAR)
                ?: run { cleanup(); return err("NUS RX characteristic not found") }
            txChar = nusService.getCharacteristic(BleUuids.NUS_TX_CHAR)
                ?: run { cleanup(); return err("NUS TX characteristic not found") }
            Log.i(TAG, "Phase 3 COMPLETE: NUS found")

            // ── Phase 5: Subscribe to TX notifications (now authenticated) ──
            emitProgress("subscribing")
            Log.i(TAG, "Phase 5: Enabling TX notifications...")
            descriptorLatch = CountDownLatch(1)
            gatt?.setCharacteristicNotification(txChar!!, true)
            txChar!!.getDescriptor(BleUuids.CCCD)?.let { desc ->
                gatt?.let { activeGatt ->
                    writeDescriptorCompat(
                        activeGatt,
                        desc,
                        BluetoothGattDescriptor.ENABLE_NOTIFICATION_VALUE
                    )
                }
                descriptorLatch!!.await(10, TimeUnit.SECONDS)
            }
            Log.i(TAG, "Phase 5 COMPLETE: TX notifications enabled")

            // ── Phase 6: TCP bridge ──
            emitProgress("bridge")
            running.set(true)
            serverSocket = ServerSocket(localPort, 1, java.net.InetAddress.getByName("127.0.0.1"))
            Log.i(TAG, "Phase 6 COMPLETE: TCP bridge on 127.0.0.1:$localPort")
            Log.i(TAG, "=== BLE CONNECT SUCCESS ===")
            emitProgress("ready")
            return null

        } catch (e: Exception) {
            cleanup()
            return err("Exception: ${e.javaClass.simpleName}: ${e.message}")
        }
    }

    /** Run service discovery against the current gatt connection with a timeout. */
    private fun discoverServicesWithLatch(timeoutSec: Long): Boolean {
        servicesLatch = CountDownLatch(1)
        servicesStatus.set(false)
        handler.post { gatt?.discoverServices() }
        val timedOut = !servicesLatch!!.await(timeoutSec, TimeUnit.SECONDS)
        if (timedOut) {
            Log.w(TAG, "Service discovery timed out after ${timeoutSec}s")
            return false
        }
        return servicesStatus.get()
    }

    fun startForwarding() {
        try {
            serverSocket?.soTimeout = 15000
            clientSocket = serverSocket?.accept()
            tcpOut = clientSocket?.getOutputStream()
            val tcpIn = clientSocket?.getInputStream() ?: return
            Log.i(TAG, "Rust TCP connected — forwarding active")

            forwardThread = Thread({ forwardTcpToBle(tcpIn) }, "ble-tcp-fwd").apply {
                isDaemon = true
                start()
            }
        } catch (e: Exception) {
            Log.e(TAG, "TCP accept failed: ${e.javaClass.simpleName}: ${e.message}")
            cleanup()
        }
    }

    @SuppressLint("MissingPermission")
    private fun forwardTcpToBle(tcpIn: InputStream) {
        // Large read buffer so one tcpIn.read() can absorb a full RNS packet;
        // BLE writes are chunked below at negotiatedMtu (device-specific payload).
        val readBuf = ByteArray(TCP_READ_BUFFER)
        try {
            while (running.get()) {
                val n = tcpIn.read(readBuf)
                if (n <= 0) break
                val rxC = rxChar ?: break
                var off = 0
                val chunkSize = negotiatedMtu.coerceAtLeast(MTU_FALLBACK_PAYLOAD)
                while (off < n && running.get()) {
                    val end = minOf(off + chunkSize, n)
                    gatt?.let { activeGatt ->
                        writeCharacteristicCompat(
                            activeGatt,
                            rxC,
                            readBuf.copyOfRange(off, end),
                            BluetoothGattCharacteristic.WRITE_TYPE_NO_RESPONSE
                        )
                    }
                    off = end
                    if (off < n) Thread.sleep(5)
                }
            }
        } catch (e: Exception) {
            if (running.get()) Log.e(TAG, "TCP→BLE error: ${e.javaClass.simpleName}: ${e.message}")
        }
        sendRnodeDetachBestEffort("TCP bridge closing")
        Log.i(TAG, "TCP→BLE stopped")
        running.set(false)
        cleanup()
    }

    @SuppressLint("MissingPermission")
    fun disconnect() {
        sendRnodeDetachBestEffort("disconnect requested")
        cleanup()
    }
    fun isRunning(): Boolean = running.get()

    @SuppressLint("MissingPermission")
    private fun sendRnodeDetachBestEffort(reason: String) {
        val rxC = rxChar ?: return
        val g = gatt ?: return
        try {
            Log.i(TAG, "Sending RNode detach before BLE close ($reason)")
            var off = 0
            val chunkSize = negotiatedMtu.coerceAtLeast(MTU_FALLBACK_PAYLOAD)
            while (off < RNODE_DETACH_FRAME.size) {
                val end = minOf(off + chunkSize, RNODE_DETACH_FRAME.size)
                writeCharacteristicCompat(
                    g,
                    rxC,
                    RNODE_DETACH_FRAME.copyOfRange(off, end),
                    BluetoothGattCharacteristic.WRITE_TYPE_NO_RESPONSE
                )
                off = end
                if (off < RNODE_DETACH_FRAME.size) Thread.sleep(5)
            }
            Thread.sleep(80)
        } catch (e: Exception) {
            Log.d(TAG, "detach($reason): ${e.javaClass.simpleName}: ${e.message}")
        }
    }

    @SuppressLint("MissingPermission")
    private fun cleanup() {
        Log.i(TAG, "cleanup()")
        running.set(false)
        unregisterBondReceiver()
        try { clientSocket?.close() } catch (e: Exception) { logEx("clientSocket.close", e) }
        try { serverSocket?.close() } catch (e: Exception) { logEx("serverSocket.close", e) }
        forwardThread?.interrupt()
        handler.post {
            try { txChar?.let { gatt?.setCharacteristicNotification(it, false) } }
            catch (e: Exception) { logEx("setCharacteristicNotification", e) }
            try { gatt?.disconnect() } catch (e: Exception) { logEx("gatt.disconnect", e) }
            try { gatt?.close() } catch (e: Exception) { logEx("gatt.close", e) }
            gatt = null
        }
        rxChar = null; txChar = null; clientSocket = null; serverSocket = null; tcpOut = null
    }

    @SuppressLint("MissingPermission")
    private fun waitForBondState(device: BluetoothDevice): Boolean {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(BOND_TIMEOUT_SEC)
        while (System.nanoTime() < deadline) {
            val state = device.bondState
            if (state == BluetoothDevice.BOND_BONDED || state == BluetoothDevice.BOND_NONE) return true
            val remainingMs = TimeUnit.NANOSECONDS
                .toMillis(deadline - System.nanoTime())
                .coerceAtLeast(1L)
            if (bondLatch?.await(minOf(BOND_POLL_INTERVAL_MS, remainingMs), TimeUnit.MILLISECONDS) == true) {
                return true
            }
        }
        return false
    }

    private fun registerBondReceiver(address: String) {
        bondReceiver = object : BroadcastReceiver() {
            override fun onReceive(ctx: Context, intent: Intent) {
                if (intent.action != BluetoothDevice.ACTION_BOND_STATE_CHANGED) return
                val dev = bluetoothDeviceExtra(intent, BluetoothDevice.EXTRA_DEVICE)
                val state = intent.getIntExtra(BluetoothDevice.EXTRA_BOND_STATE, -1)
                val prev = intent.getIntExtra(BluetoothDevice.EXTRA_PREVIOUS_BOND_STATE, -1)
                Log.i(TAG, "Bond: ${bondStr(prev)} → ${bondStr(state)} (${dev?.address})")
                if (dev?.address == address) {
                    if (state == BluetoothDevice.BOND_BONDED || state == BluetoothDevice.BOND_NONE) {
                        bondLatch?.countDown()
                    }
                }
            }
        }
        val filter = IntentFilter(BluetoothDevice.ACTION_BOND_STATE_CHANGED)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            context.registerReceiver(bondReceiver, filter, Context.RECEIVER_NOT_EXPORTED)
        } else {
            context.registerReceiver(bondReceiver, filter)
        }
    }

    private fun unregisterBondReceiver() {
        bondReceiver?.let {
            try { context.unregisterReceiver(it) }
            catch (e: Exception) { logEx("unregisterReceiver", e) }
        }
        bondReceiver = null
    }

    private fun err(msg: String): String { Log.e(TAG, msg); return msg }

    private fun logEx(where: String, e: Exception) {
        Log.d(TAG, "cleanup($where): ${e.javaClass.simpleName}: ${e.message}")
    }

    /** Push a connection-phase update to JS for UI progress. */
    private fun emitProgress(phase: String) {
        val wv = webViewRef ?: return
        handler.post {
            wv.evaluateJavascript(
                "if(typeof window._onBleConnectProgress==='function')window._onBleConnectProgress('$phase');",
                null
            )
        }
    }

    private fun bondStr(s: Int) = when (s) {
        BluetoothDevice.BOND_NONE -> "NONE"
        BluetoothDevice.BOND_BONDING -> "BONDING"
        BluetoothDevice.BOND_BONDED -> "BONDED"
        else -> "?($s)"
    }

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

    @Suppress("DEPRECATION")
    private fun bluetoothDeviceExtra(intent: Intent, name: String): BluetoothDevice? {
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            intent.getParcelableExtra(name, BluetoothDevice::class.java)
        } else {
            intent.getParcelableExtra(name)
        }
    }

    private fun handleCharacteristicChanged(ch: BluetoothGattCharacteristic, data: ByteArray) {
        if (ch.uuid == BleUuids.NUS_TX_CHAR) {
            try { tcpOut?.write(data); tcpOut?.flush() }
            catch (e: Exception) {
                if (running.get()) {
                    Log.e(TAG, "BLE→TCP: ${e.javaClass.simpleName}: ${e.message}")
                    running.set(false)
                }
            }
        }
    }

    private val gattCallback = object : BluetoothGattCallback() {
        override fun onConnectionStateChange(gatt: BluetoothGatt, status: Int, newState: Int) {
            val s = when (newState) {
                BluetoothProfile.STATE_CONNECTED -> "CONNECTED"
                BluetoothProfile.STATE_DISCONNECTED -> "DISCONNECTED"
                else -> "OTHER($newState)"
            }
            Log.i(TAG, "GATT: status=$status state=$s bondState=${bondStr(gatt.device?.bondState ?: -1)}")

            when (newState) {
                BluetoothProfile.STATE_CONNECTED -> {
                    connectStatus.set(true)
                    connectLatch?.countDown()
                }
                BluetoothProfile.STATE_DISCONNECTED -> {
                    connectStatus.set(false)
                    val bondState = gatt.device?.bondState ?: BluetoothDevice.BOND_NONE
                    if (bondState != BluetoothDevice.BOND_BONDING) {
                        connectLatch?.countDown()
                    } else {
                        Log.i(TAG, "GATT disconnected during bonding — bond receiver will handle reconnect")
                    }
                    if (running.getAndSet(false)) {
                        Log.w(TAG, "BLE disconnected while bridge active")
                        try { clientSocket?.close() }
                        catch (e: Exception) { logEx("clientSocket.close on disconnect", e) }
                    }
                }
            }
        }

        override fun onMtuChanged(gatt: BluetoothGatt, mtu: Int, status: Int) {
            if (status == BluetoothGatt.GATT_SUCCESS) negotiatedMtu = mtu - 3
            Log.i(TAG, "MTU: $mtu (payload=$negotiatedMtu) status=$status")
            mtuLatch?.countDown()
        }

        override fun onServicesDiscovered(gatt: BluetoothGatt, status: Int) {
            servicesStatus.set(status == BluetoothGatt.GATT_SUCCESS)
            Log.i(TAG, "Services discovered: status=$status ok=${servicesStatus.get()}")
            servicesLatch?.countDown()
        }

        override fun onDescriptorWrite(gatt: BluetoothGatt, desc: BluetoothGattDescriptor, status: Int) {
            Log.i(TAG, "Descriptor write: ${desc.uuid} status=$status")
            descriptorLatch?.countDown()
        }

        override fun onCharacteristicChanged(
            gatt: BluetoothGatt,
            ch: BluetoothGattCharacteristic,
            value: ByteArray
        ) {
            handleCharacteristicChanged(ch, value)
        }

        @Suppress("DEPRECATION", "OVERRIDE_DEPRECATION")
        override fun onCharacteristicChanged(gatt: BluetoothGatt, ch: BluetoothGattCharacteristic) {
            handleCharacteristicChanged(ch, ch.value ?: return)
        }
    }
}
