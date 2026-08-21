package org.ratspeak.android

import android.annotation.SuppressLint
import android.bluetooth.BluetoothManager
import android.content.Context
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Process owner for Android's native BLE RNode bridge.
 *
 * The stable listener and operation fence outlive each physical GATT
 * generation. Unexpected radio loss closes only the accepted bridge client;
 * explicit teardown tombstones the exact operation before closing everything.
 */
@SuppressLint("StaticFieldLeak") // Entries hold only applicationContext; never Activity/View.
object RatspeakBleRnodeSupervisor {
    private const val TAG = "RatspeakBleSupervisor"
    private val tokenRegex = Regex("^[0-9A-Fa-f]{32}$")
    private val addressRegex = Regex("^([0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}$")
    private val lock = Any()
    private val replacementLock = Any()
    private val retryWake = Object()
    private val autoResume = AtomicBoolean(true)
    private var current: Entry? = null

    const val FAILURE_AUTO_RESUME_DISABLED = "auto_resume_disabled"

    private enum class RetryWaitOutcome {
        ELAPSED,
        ADAPTER_RECOVERED,
        STOPPED,
    }

    @JvmStatic
    fun setAutoResume(enabled: Boolean) {
        autoResume.set(enabled)
        synchronized(retryWake) { retryWake.notifyAll() }
    }

    private class Entry(
        val context: Context,
        val address: String,
        val localPort: Int,
        val token: String,
        val generation: Long,
        val listener: ServerSocket,
    ) {
        val active = AtomicBoolean(true)
        val terminalPublished = AtomicBoolean(false)
        val publishLock = Any()
        @Volatile var physical: RatspeakBleGatt? = null
        @Volatile var worker: Thread? = null
    }

    @JvmStatic
    fun startOrReplace(
        context: Context,
        address: String,
        localPort: Int,
        operationToken: String,
        installedGeneration: Long,
    ): Boolean {
        if (!tokenRegex.matches(operationToken) ||
            !addressRegex.matches(address) ||
            localPort !in 1..65535 ||
            installedGeneration < 0
        ) {
            return false
        }

        synchronized(replacementLock) {
            val observed = synchronized(lock) { current }
            val requested = RatspeakMobilePolicy.BleOperation(
                operationToken,
                installedGeneration,
                address,
                localPort,
            )
            val plan = RatspeakMobilePolicy.bleReplacementPlan(
                observed?.takeIf { it.active.get() }?.let {
                    RatspeakMobilePolicy.BleOperation(
                        it.token,
                        it.generation,
                        it.address,
                        it.localPort,
                    )
                },
                requested,
            )
            if (plan == RatspeakMobilePolicy.BleReplacementPlan.IDEMPOTENT) return true
            val mustDisplaceFirst =
                plan == RatspeakMobilePolicy.BleReplacementPlan.DISPLACE_THEN_BIND
            if (mustDisplaceFirst) {
                // Two acceptors cannot safely share one listener. A same-port
                // replace therefore has explicit destructive semantics:
                // tombstone and close the exact old operation before rebinding.
                val old = synchronized(lock) {
                    if (current !== observed) return false
                    current = null
                    observed?.active?.set(false)
                    observed
                }
                old?.let(::closeEntry)
            }
            val listener = try {
                // Different-port replacement is transactional: bind first so
                // rejection leaves the current radio intact.
                ServerSocket().apply {
                    reuseAddress = true
                    // Keep admission bounded; the bridge filters any closed
                    // generation before admitting the next live connector.
                    bind(InetSocketAddress(InetAddress.getByName("127.0.0.1"), localPort), 1)
                }
            } catch (error: Throwable) {
                Log.w(TAG, "Local BLE bridge listener unavailable")
                return false
            }

            val replacement = Entry(
                context.applicationContext,
                address,
                localPort,
                operationToken,
                installedGeneration,
                listener,
            )
            val displaced = synchronized(lock) {
                val old = current
                if (!mustDisplaceFirst && old !== observed) {
                    try { listener.close() } catch (_: Throwable) {}
                    return false
                }
                current = replacement
                old?.active?.set(false)
                old
            }
            displaced?.let(::closeEntry)
            publishListenerReady(replacement)
            replacement.worker = Thread({ run(replacement) }, "ble-rnode-supervisor").apply {
                isDaemon = true
                start()
            }
            return true
        }
    }

    @JvmStatic
    fun disconnect(operationToken: String?, installedGeneration: Long): Boolean {
        synchronized(replacementLock) {
            val removed = synchronized(lock) {
                val candidate = current ?: return false
                if (operationToken != null && candidate.token != operationToken) return false
                if (candidate.generation != installedGeneration) return false
                current = null
                // Tombstone first: callbacks from close/disconnect cannot publish.
                candidate.active.set(false)
                candidate
            }
            closeEntry(removed)
            RatspeakNativeBridge.publishBleState(
                removed.token,
                removed.generation,
                RatspeakNativeBridge.BLE_DISABLED,
                0,
                null,
            )
            return true
        }
    }

    private fun run(entry: Entry) {
        var retry = 0
        while (isCurrent(entry)) {
            publishState(entry, RatspeakNativeBridge.BLE_CONNECTING, null)
            publishProgress(entry, if (retry == 0) "connecting" else "connecting_retry")
            val physical = RatspeakBleGatt(entry.context) { phase -> publishProgress(entry, phase) }
            entry.physical = physical
            if (!isCurrent(entry)) {
                physical.disconnect(graceful = false)
                break
            }

            val error = physical.connect(entry.address)
            if (error != null) {
                entry.physical = null
                physical.disconnect(graceful = false)
                if (!isCurrent(entry)) break
                if (!RatspeakMobilePolicy.shouldRetryBle(error.code)) {
                    finishTerminal(entry, error.code)
                    break
                }
                if (!RatspeakMobilePolicy.shouldAutoResumeBle(error.code, autoResume.get())) {
                    finishTerminal(entry, FAILURE_AUTO_RESUME_DISABLED)
                    break
                }
                publishRetry(entry, error.code)
                val retryOutcome = if (
                    error.code == RatspeakBleGatt.FAILURE_BLUETOOTH_OFF ||
                    bluetoothEnabled(entry.context) == false
                ) {
                    if (waitForBluetooth(entry)) {
                        RetryWaitOutcome.ADAPTER_RECOVERED
                    } else {
                        RetryWaitOutcome.STOPPED
                    }
                } else {
                    waitForRetry(entry, retry)
                }
                if (retryOutcome == RetryWaitOutcome.STOPPED) {
                    finishManualHoldIfNeeded(entry)
                    break
                }
                retry = if (retryOutcome == RetryWaitOutcome.ADAPTER_RECOVERED) 0 else retry + 1
                continue
            }

            retry = 0
            if (!isCurrent(entry)) {
                physical.disconnect(graceful = false)
                break
            }
            publishReady(entry)
            physical.startForwarding(entry.listener)
            physical.awaitStopped()
            entry.physical = null
            physical.disconnect(graceful = false)
            if (!isCurrent(entry)) break
            if (!autoResume.get()) {
                finishTerminal(entry, FAILURE_AUTO_RESUME_DISABLED)
                break
            }
            publishRetry(entry, "radio_disconnected")
            val retryOutcome = if (bluetoothEnabled(entry.context) == false) {
                if (waitForBluetooth(entry)) {
                    RetryWaitOutcome.ADAPTER_RECOVERED
                } else {
                    RetryWaitOutcome.STOPPED
                }
            } else {
                waitForRetry(entry, retry)
            }
            if (retryOutcome == RetryWaitOutcome.STOPPED) {
                finishManualHoldIfNeeded(entry)
                break
            }
            retry = if (retryOutcome == RetryWaitOutcome.ADAPTER_RECOVERED) 0 else retry + 1
        }
        entry.physical = null
        try { entry.listener.close() } catch (_: Throwable) {}
    }

    private fun publishProgress(entry: Entry, phase: String) {
        synchronized(entry.publishLock) {
            if (!isCurrent(entry)) return
            RatspeakAndroidObservers.bleProgress(entry.token, entry.generation, phase)
        }
    }

    private fun publishReady(entry: Entry) {
        synchronized(entry.publishLock) {
            if (!isCurrent(entry)) return
            RatspeakNativeBridge.publishBleState(
                entry.token,
                entry.generation,
                RatspeakNativeBridge.BLE_CONNECTED,
                0,
                null,
            )
        }
    }

    private fun publishRetry(entry: Entry, reason: String) {
        synchronized(entry.publishLock) {
            if (!isCurrent(entry)) return
            RatspeakNativeBridge.publishBleState(
                entry.token,
                entry.generation,
                RatspeakNativeBridge.BLE_RECONNECTING,
                0,
                closedBleError(reason),
            )
            RatspeakAndroidObservers.bleProgress(entry.token, entry.generation, "connecting_retry")
        }
    }

    private fun publishListenerReady(entry: Entry) {
        synchronized(entry.publishLock) {
            if (!isCurrent(entry)) return
            RatspeakNativeBridge.publishBleState(
                entry.token,
                entry.generation,
                RatspeakNativeBridge.BLE_LISTENER_READY,
                entry.localPort,
                null,
            )
        }
    }

    private fun publishState(entry: Entry, state: Int, error: String?) {
        synchronized(entry.publishLock) {
            if (!isCurrent(entry)) return
            RatspeakNativeBridge.publishBleState(entry.token, entry.generation, state, 0, error)
        }
    }

    private fun finishTerminal(entry: Entry, code: String) {
        synchronized(entry.publishLock) {
            val removed = synchronized(lock) {
                if (current !== entry) return
                current = null
                entry.active.set(false)
                entry
            }
            try { removed.listener.close() } catch (_: Throwable) {}
            try { removed.physical?.disconnect(graceful = false) } catch (_: Throwable) {}
            if (removed.terminalPublished.compareAndSet(false, true)) {
                RatspeakNativeBridge.publishBleState(
                    removed.token,
                    removed.generation,
                    RatspeakNativeBridge.BLE_FAILED,
                    0,
                    code,
                )
            }
        }
    }

    private fun closedBleError(reason: String): String {
        return when (reason) {
            "radio_disconnected",
            RatspeakBleGatt.FAILURE_BLUETOOTH_OFF,
            RatspeakBleGatt.FAILURE_PERMISSION,
            RatspeakBleGatt.FAILURE_CONNECT,
            FAILURE_AUTO_RESUME_DISABLED -> reason
            else -> RatspeakBleGatt.FAILURE_CONNECT
        }
    }

    private fun waitForRetry(entry: Entry, attempt: Int): RetryWaitOutcome {
        var remaining = RatspeakMobilePolicy.bleReconnectDelayMs(attempt, entry.token.hashCode())
        while (remaining > 0 && isCurrent(entry) && autoResume.get()) {
            if (bluetoothEnabled(entry.context) == false) {
                return if (waitForBluetooth(entry)) {
                    RetryWaitOutcome.ADAPTER_RECOVERED
                } else {
                    RetryWaitOutcome.STOPPED
                }
            }
            val slice = minOf(remaining, 250L)
            try {
                synchronized(retryWake) { retryWake.wait(slice) }
            } catch (_: InterruptedException) {
                Thread.currentThread().interrupt()
                return RetryWaitOutcome.STOPPED
            }
            remaining -= slice
        }
        return if (isCurrent(entry) && autoResume.get()) {
            RetryWaitOutcome.ELAPSED
        } else {
            RetryWaitOutcome.STOPPED
        }
    }

    /**
     * Adapter-off is a state wait, not a failed connection attempt. Polling at
     * a low fixed cadence also covers vendor builds that suppress the adapter
     * state broadcast; policy changes wake it immediately.
     */
    private fun waitForBluetooth(entry: Entry): Boolean {
        while (isCurrent(entry) && autoResume.get()) {
            if (bluetoothEnabled(entry.context) != false) return true
            try {
                synchronized(retryWake) { retryWake.wait(250L) }
            } catch (_: InterruptedException) {
                Thread.currentThread().interrupt()
                return false
            }
        }
        return false
    }

    private fun bluetoothEnabled(context: Context): Boolean? {
        return try {
            context.getSystemService(BluetoothManager::class.java)?.adapter?.isEnabled
        } catch (_: SecurityException) {
            null
        }
    }

    private fun finishManualHoldIfNeeded(entry: Entry) {
        if (isCurrent(entry) && !autoResume.get()) {
            finishTerminal(entry, FAILURE_AUTO_RESUME_DISABLED)
        }
    }

    private fun isCurrent(entry: Entry): Boolean {
        if (!entry.active.get()) return false
        return synchronized(lock) { current === entry }
    }

    private fun closeEntry(entry: Entry) {
        // The caller has already tombstoned the entry.
        // Gracefully detach the physical radio before closing the listener. Closing
        // the listener first can wake the forwarding loop and race its cleanup
        // against the fallback detach frame.
        try { entry.physical?.disconnect(graceful = true) } catch (_: Throwable) {}
        try { entry.listener.close() } catch (_: Throwable) {}
        entry.worker?.interrupt()
        synchronized(retryWake) { retryWake.notifyAll() }
    }
}
