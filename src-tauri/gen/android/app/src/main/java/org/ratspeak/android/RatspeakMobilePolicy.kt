package org.ratspeak.android

import java.io.ByteArrayOutputStream
import java.io.OutputStream
import kotlin.math.absoluteValue

/** Pure Android-mobile policy helpers kept free of framework dependencies. */
object RatspeakMobilePolicy {
    const val DEFAULT_ATT_PAYLOAD = 20
    const val RNODE_GATT_ENQUEUE_TIMEOUT_MS = 1_200L
    const val RNODE_GATT_CALLBACK_TIMEOUT_MS = 5_000L
    const val RNODE_GATT_WRITE_PACING_MS = 12L
    const val RNODE_MAX_ESCAPED_FRAME_BYTES = 508 * 2 + 3
    private const val MAX_ATT_PAYLOAD = 514

    // Closed localhost-only protocol used by the native BLE bridge. The RNode
    // never sees this command: Kotlin emits it only toward Rust after a complete
    // KISS data frame has crossed an acknowledged GATT write boundary.
    const val NATIVE_BRIDGE_ACK_COMMAND = 0xA0
    private const val KISS_DATA_COMMAND = 0x00
    private const val KISS_FEND = 0xC0
    private const val KISS_FESC = 0xDB
    private const val KISS_TFEND = 0xDC
    private const val KISS_TFESC = 0xDD
    private val NATIVE_BRIDGE_ACK_MAGIC = byteArrayOf(0x52, 0x53, 0x42, 0x41, 0x01)

    enum class RnodeWriteMode {
        WITH_RESPONSE,
        UNSUPPORTED,
    }

    /** Reliability requires the acknowledged PROPERTY_WRITE contract. */
    fun rnodeWriteMode(hasWrite: Boolean): RnodeWriteMode {
        return if (hasWrite) RnodeWriteMode.WITH_RESPONSE else RnodeWriteMode.UNSUPPORTED
    }

    data class NativeBridgeKissRecord(
        val wire: ByteArray,
        val command: Int,
    ) {
        val isData: Boolean get() = command == KISS_DATA_COMMAND
    }

    data class NativeBridgeRecordAssembly(
        val records: List<NativeBridgeKissRecord>,
        val overflow: Boolean,
    )

    /**
     * Physical-GATT-generation KISS assembler for BLE notifications. Only
     * complete records leave this class, so TCP replacement can replay a whole
     * ambiguous record and a new Rust deframer never sees an orphaned suffix.
     */
    class NativeBridgeInboundKissAssembler(
        private val maxFrameBytes: Int = RNODE_MAX_ESCAPED_FRAME_BYTES,
    ) {
        private val frame = ByteArrayOutputStream()
        private var started = false
        private var failed = false

        init {
            require(maxFrameBytes >= 3)
        }

        fun offer(bytes: ByteArray, start: Int = 0, end: Int = bytes.size): NativeBridgeRecordAssembly {
            require(start in 0..end && end <= bytes.size)
            if (failed) return NativeBridgeRecordAssembly(emptyList(), overflow = true)
            val records = mutableListOf<NativeBridgeKissRecord>()
            for (index in start until end) {
                val value = bytes[index].toInt() and 0xFF
                if (!started) {
                    if (value == KISS_FEND) {
                        frame.reset()
                        frame.write(value)
                        started = true
                    }
                    continue
                }

                if (value == KISS_FEND && frame.size() == 1) {
                    // Collapse idle/consecutive boundaries while retaining one
                    // start delimiter for the next real record.
                    continue
                }
                if (frame.size() >= maxFrameBytes) {
                    failed = true
                    return NativeBridgeRecordAssembly(records, overflow = true)
                }
                frame.write(value)
                if (value == KISS_FEND) {
                    val wire = frame.toByteArray()
                    records += NativeBridgeKissRecord(wire, decodeKissCommand(wire))
                    frame.reset()
                    // RawKissDeframer treats a terminal FEND as the start of
                    // the next record too. Retain a private copy so each queued
                    // record remains independently replayable.
                    frame.write(KISS_FEND)
                    started = true
                }
            }
            return NativeBridgeRecordAssembly(records, overflow = false)
        }

        fun reset() {
            frame.reset()
            started = false
            failed = false
        }
    }

    data class NativeBridgeBleWriteChunk(
        val wire: ByteArray,
        val completedDataFrames: Int,
    )

    data class NativeBridgeOutboundOffer(
        val chunks: List<NativeBridgeBleWriteChunk>,
        val overflow: Boolean,
    )

    /**
     * Streaming Rust-to-BLE coalescer. TCP fragmentation never determines
     * GATT write count: chunks are full ATT payloads except for the final chunk
     * of a complete KISS record, and DATA completion is attached only there.
     */
    class NativeBridgeOutboundKissCoalescer(
        private val chunkBytes: Int,
        private val maxFrameBytes: Int = RNODE_MAX_ESCAPED_FRAME_BYTES,
    ) {
        private val pending = ByteArrayOutputStream()
        private var started = false
        private var frameBytes = 0
        private var command = -1
        private var commandEscape = false
        private var failed = false

        init {
            require(chunkBytes >= 2)
            require(maxFrameBytes >= 3)
        }

        fun offer(bytes: ByteArray, start: Int = 0, end: Int = bytes.size): NativeBridgeOutboundOffer {
            require(start in 0..end && end <= bytes.size)
            if (failed) return NativeBridgeOutboundOffer(emptyList(), overflow = true)
            val chunks = mutableListOf<NativeBridgeBleWriteChunk>()
            for (index in start until end) {
                val value = bytes[index].toInt() and 0xFF
                if (!started) {
                    if (value == KISS_FEND) {
                        pending.reset()
                        pending.write(value)
                        started = true
                        frameBytes = 1
                        command = -1
                        commandEscape = false
                    }
                    continue
                }

                if (value == KISS_FEND && frameBytes == 1) {
                    continue
                }
                // A valid maximum-sized record must still have room for this
                // delimiter. A non-delimiter at the limit is already invalid.
                if (frameBytes >= maxFrameBytes ||
                    (value != KISS_FEND && frameBytes + 1 >= maxFrameBytes)
                ) {
                    failed = true
                    pending.reset()
                    return NativeBridgeOutboundOffer(chunks, overflow = true)
                }

                pending.write(value)
                frameBytes++
                if (command < 0 && value != KISS_FEND) {
                    if (commandEscape) {
                        command = when (value) {
                            KISS_TFEND -> KISS_FEND
                            KISS_TFESC -> KISS_FESC
                            else -> value
                        }
                        commandEscape = false
                    } else if (value == KISS_FESC) {
                        commandEscape = true
                    } else {
                        command = value
                    }
                }

                if (value == KISS_FEND) {
                    chunks += NativeBridgeBleWriteChunk(
                        pending.toByteArray(),
                        completedDataFrames = if (command == KISS_DATA_COMMAND) 1 else 0,
                    )
                    pending.reset()
                    // Preserve shared-boundary KISS streams. The duplicate is
                    // emitted only if another record actually follows and is
                    // harmless to the RNode parser.
                    pending.write(KISS_FEND)
                    started = true
                    frameBytes = 1
                    command = -1
                    commandEscape = false
                } else if (pending.size() == chunkBytes) {
                    chunks += NativeBridgeBleWriteChunk(
                        pending.toByteArray(),
                        completedDataFrames = 0,
                    )
                    pending.reset()
                }
            }
            return NativeBridgeOutboundOffer(chunks, overflow = false)
        }
    }

    /**
     * Causality fence between localhost Rust generations. RNode controls are
     * immediate responses, so a short quiet window separates old responses
     * from new requests. DATA never participates: it belongs to physical GATT.
     * The final install action runs under the same tiny monitor as control
     * observation, closing the check/install race without doing socket I/O.
     */
    class NativeBridgeControlQuiescence(
        quietIntervalNanos: Long,
        hardMaxNanos: Long,
        private val nowNanos: () -> Long = System::nanoTime,
        private val waitNanos: (Long) -> Unit = { duration ->
            val millis = duration / 1_000_000L
            val nanos = (duration % 1_000_000L).toInt()
            Thread.sleep(millis, nanos)
        },
    ) {
        enum class Outcome { READY, HARD_FAILURE, STOPPED, ACTION_FAILED }

        inner class Boundary internal constructor(
            internal val closedAt: Long,
            internal val hardDeadline: Long,
        ) {
            fun awaitQuiet(shouldContinue: () -> Boolean): Outcome =
                awaitBoundary(this, shouldContinue, null)

            fun awaitQuietAndCommit(
                shouldContinue: () -> Boolean,
                action: () -> Boolean,
            ): Outcome = awaitBoundary(this, shouldContinue, action)
        }

        private val quietInterval = quietIntervalNanos
        private val hardMax = hardMaxNanos
        private val monitor = Object()
        private var latestControl = Long.MIN_VALUE

        init {
            require(quietIntervalNanos > 0)
            require(hardMaxNanos >= quietIntervalNanos)
        }

        fun observeCompleteRecord(isData: Boolean) {
            if (isData) return
            synchronized(monitor) {
                latestControl = nowNanos()
            }
        }

        fun beginAfterClosure(): Boundary {
            val closedAt = nowNanos()
            return Boundary(closedAt, saturatingAdd(closedAt, hardMax))
        }

        private fun awaitBoundary(
            boundary: Boundary,
            shouldContinue: () -> Boolean,
            action: (() -> Boolean)?,
        ): Outcome {
            while (true) {
                var outcome: Outcome? = null
                var delay = 0L
                synchronized(monitor) {
                    if (!shouldContinue()) {
                        outcome = Outcome.STOPPED
                    } else {
                        val now = nowNanos()
                        val quietBase = maxOf(boundary.closedAt, latestControl)
                        val readyAt = saturatingAdd(quietBase, quietInterval)
                        if (now >= readyAt) {
                            outcome = if (action == null || action()) {
                                Outcome.READY
                            } else {
                                Outcome.ACTION_FAILED
                            }
                        } else if (now >= boundary.hardDeadline) {
                            outcome = Outcome.HARD_FAILURE
                        } else {
                            delay = minOf(readyAt, boundary.hardDeadline) - now
                        }
                    }
                }
                outcome?.let { return it }
                try {
                    waitNanos(delay.coerceAtLeast(1L))
                } catch (_: InterruptedException) {
                    Thread.currentThread().interrupt()
                    return Outcome.STOPPED
                }
            }
        }

        private fun saturatingAdd(value: Long, increment: Long): Long {
            return if (value > Long.MAX_VALUE - increment) Long.MAX_VALUE else value + increment
        }
    }

    /**
     * Single blocking-I/O owner for the local TCP bridge. Lifecycle methods
     * only mutate state under the monitor and close sockets outside it; the
     * writer performs OutputStream I/O with no lifecycle lock held.
     */
    class NativeBridgeTcpWriter<T : Any>(
        private val maxInboundRecords: Int,
        private val maxInboundBytes: Int,
        private val maxControlRecords: Int = maxInboundRecords,
        private val maxControlBytes: Int = maxInboundBytes,
        private val maxAckRecords: Int,
        private val closeSocket: (T) -> Unit,
        threadName: String = "ble-tcp-writer",
    ) {
        private enum class Kind { DATA, CONTROL, ACK }

        private data class Entry<T : Any>(
            val kind: Kind,
            val wire: ByteArray,
            val expectedSocket: T? = null,
        )

        private data class Work<T : Any>(
            val entry: Entry<T>,
            val socket: T,
            val output: OutputStream,
        )

        private val monitor = Object()
        private val queue = ArrayDeque<Entry<T>>()
        private var inboundRecords = 0
        private var inboundBytes = 0
        private var controlRecords = 0
        private var controlBytes = 0
        private var ackRecords = 0
        private var activeSocket: T? = null
        private var activeOutput: OutputStream? = null
        private var running = true
        private val writerThread = Thread({ writerLoop() }, threadName).apply {
            isDaemon = true
            start()
        }

        init {
            require(maxInboundRecords > 0)
            require(maxInboundBytes > 0)
            require(maxControlRecords > 0)
            require(maxControlBytes > 0)
            require(maxAckRecords > 0)
        }

        fun install(socket: T, output: OutputStream): Boolean {
            val previous = synchronized(monitor) {
                if (!running) return@synchronized null
                val old = activeSocket
                activeSocket = socket
                activeOutput = output
                monitor.notifyAll()
                old
            }
            if (!isRunning()) return false
            if (previous != null && previous !== socket) safeClose(previous)
            return true
        }

        /** Short state-only install used by the quiescence commit lock. */
        fun installVacant(socket: T, output: OutputStream): Boolean = synchronized(monitor) {
            if (!running || activeSocket != null) return@synchronized false
            activeSocket = socket
            activeOutput = output
            monitor.notifyAll()
            true
        }

        fun enqueueInbound(record: ByteArray): Boolean = synchronized(monitor) {
            if (!running || record.isEmpty() || inboundRecords >= maxInboundRecords ||
                record.size > maxInboundBytes - inboundBytes
            ) {
                return@synchronized false
            }
            queue.addLast(Entry(Kind.DATA, record.copyOf()))
            inboundRecords++
            inboundBytes += record.size
            monitor.notifyAll()
            true
        }

        /**
         * Controls are observations of one Rust protocol generation. Bind to
         * the active socket now; without one, discard instead of allowing a
         * later generation to inherit stale readiness evidence.
         */
        fun enqueueControl(record: ByteArray): Boolean = synchronized(monitor) {
            if (!running || record.isEmpty()) return@synchronized false
            val socket = activeSocket ?: return@synchronized true
            if (controlRecords >= maxControlRecords ||
                record.size > maxControlBytes - controlBytes
            ) {
                return@synchronized false
            }
            queue.addLast(Entry(Kind.CONTROL, record.copyOf(), socket))
            controlRecords++
            controlBytes += record.size
            monitor.notifyAll()
            true
        }

        fun enqueueAck(expectedSocket: T, record: ByteArray): Boolean = synchronized(monitor) {
            if (!running || activeSocket !== expectedSocket || ackRecords >= maxAckRecords) {
                return@synchronized false
            }
            queue.addLast(Entry(Kind.ACK, record.copyOf(), expectedSocket))
            ackRecords++
            monitor.notifyAll()
            true
        }

        fun close(expectedSocket: T? = null) {
            val closing = synchronized(monitor) {
                val active = activeSocket
                if (expectedSocket != null && active !== expectedSocket) return@synchronized null
                activeSocket = null
                activeOutput = null
                monitor.notifyAll()
                active
            }
            if (closing != null) safeClose(closing)
        }

        fun shutdown() {
            val closing = synchronized(monitor) {
                if (!running) return@synchronized null
                running = false
                val active = activeSocket
                activeSocket = null
                activeOutput = null
                queue.clear()
                inboundRecords = 0
                inboundBytes = 0
                controlRecords = 0
                controlBytes = 0
                ackRecords = 0
                monitor.notifyAll()
                active
            }
            if (closing != null) safeClose(closing)
            if (writerThread !== Thread.currentThread()) {
                try {
                    writerThread.join(2_000)
                } catch (_: InterruptedException) {
                    Thread.currentThread().interrupt()
                }
            }
        }

        fun isRunning(): Boolean = synchronized(monitor) { running }

        fun queuedInboundRecords(): Int = synchronized(monitor) { inboundRecords }

        fun queuedAckRecords(): Int = synchronized(monitor) { ackRecords }

        fun queuedControlRecords(): Int = synchronized(monitor) { controlRecords }

        private fun writerLoop() {
            while (true) {
                val work = nextWork() ?: return
                var success = false
                try {
                    work.output.write(work.entry.wire)
                    work.output.flush()
                    success = true
                } catch (_: Throwable) {
                    // The write may be partial. DATA is requeued whole;
                    // exact-socket CONTROL/ACK is deliberately never replayed.
                }

                var closeFailedSocket = false
                synchronized(monitor) {
                    if (!running) {
                        // shutdown() already cleared the queue accounting and
                        // closed the exact socket to unblock this write.
                    } else if (success) {
                        completeEntry(work.entry)
                    } else {
                        if (work.entry.kind == Kind.DATA && running) {
                            queue.addFirst(work.entry)
                        } else {
                            completeEntry(work.entry)
                        }
                        if (activeSocket === work.socket) {
                            activeSocket = null
                            activeOutput = null
                            closeFailedSocket = true
                        }
                        monitor.notifyAll()
                    }
                }
                if (closeFailedSocket) safeClose(work.socket)
            }
        }

        private fun nextWork(): Work<T>? = synchronized(monitor) {
            while (running) {
                while (queue.isNotEmpty()) {
                    val head = queue.first()
                    val socket = activeSocket
                    val output = activeOutput
                    if (head.kind != Kind.DATA && head.expectedSocket !== socket) {
                        queue.removeFirst()
                        completeEntry(head)
                        continue
                    }
                    if (socket == null || output == null) break
                    queue.removeFirst()
                    return@synchronized Work(head, socket, output)
                }
                try {
                    monitor.wait()
                } catch (_: InterruptedException) {
                    Thread.currentThread().interrupt()
                    running = false
                }
            }
            null
        }

        private fun completeEntry(entry: Entry<T>) {
            when (entry.kind) {
                Kind.DATA -> {
                    inboundRecords--
                    inboundBytes -= entry.wire.size
                }
                Kind.CONTROL -> {
                    controlRecords--
                    controlBytes -= entry.wire.size
                }
                Kind.ACK -> ackRecords--
            }
        }

        private fun safeClose(socket: T) {
            try {
                closeSocket(socket)
            } catch (_: Throwable) {
            }
        }
    }

    private fun decodeKissCommand(wire: ByteArray): Int {
        if (wire.size < 3 || (wire[0].toInt() and 0xFF) != KISS_FEND) return -1
        val first = wire[1].toInt() and 0xFF
        if (first != KISS_FESC) return first
        if (wire.size < 4) return -1
        return when (val escaped = wire[2].toInt() and 0xFF) {
            KISS_TFEND -> KISS_FEND
            KISS_TFESC -> KISS_FESC
            else -> escaped
        }
    }

    /** Exact KISS frame Rust recognises as one completed native bridge data write. */
    fun nativeBridgeAckFrame(acknowledgedDataFrames: Long): ByteArray {
        require(acknowledgedDataFrames > 0)
        val payload = ByteArray(NATIVE_BRIDGE_ACK_MAGIC.size + Long.SIZE_BYTES)
        NATIVE_BRIDGE_ACK_MAGIC.copyInto(payload)
        for (index in 0 until Long.SIZE_BYTES) {
            val shift = (Long.SIZE_BYTES - index - 1) * 8
            payload[NATIVE_BRIDGE_ACK_MAGIC.size + index] =
                (acknowledgedDataFrames ushr shift).toByte()
        }

        val frame = ByteArrayOutputStream(payload.size + 3)
        frame.write(KISS_FEND)
        frame.write(NATIVE_BRIDGE_ACK_COMMAND)
        payload.forEach { byte ->
            when (byte.toInt() and 0xFF) {
                KISS_FEND -> {
                    frame.write(KISS_FESC)
                    frame.write(KISS_TFEND)
                }
                KISS_FESC -> {
                    frame.write(KISS_FESC)
                    frame.write(KISS_TFESC)
                }
                else -> frame.write(byte.toInt() and 0xFF)
            }
        }
        frame.write(KISS_FEND)
        return frame.toByteArray()
    }

    /** Null means the callback is lifecycle-only, not allocator pressure. */
    fun attachmentMemoryPressure(level: Int): Boolean? {
        if (level == 20 || level < 10) return null
        return level == 15 || level >= 60
    }

    /** ATT payload is MTU minus the three-byte ATT header. */
    fun attPayload(mtu: Int, negotiationSucceeded: Boolean): Int {
        if (!negotiationSucceeded) return DEFAULT_ATT_PAYLOAD
        return (mtu - 3).coerceIn(1, MAX_ATT_PAYLOAD)
    }

    /** Android permits only one outstanding server notification per peer. */
    fun mayQueueGattNotification(previousSendCompleted: Boolean): Boolean {
        return previousSendCompleted
    }

    /**
     * Fast retries handle Android's transient GATT failures; later retries
     * become low duty but never give up on an explicitly configured radio.
     * A stable token-derived jitter prevents several radios reconnecting in
     * lockstep without making tests or user-visible timing nondeterministic.
     */
    fun bleReconnectDelayMs(attempt: Int, stableSeed: Int): Long {
        val normalizedAttempt = attempt.coerceAtLeast(0)
        val base = when (normalizedAttempt) {
            0 -> 2_000L
            1 -> 5_000L
            2 -> 10_000L
            3 -> 20_000L
            4 -> 30_000L
            in 5..12 -> 60_000L
            in 13..17 -> 120_000L
            else -> 600_000L
        }
        val jitterRange = (base / 10L).coerceAtLeast(1L)
        val jitter = stableSeed.toLong().absoluteValue % jitterRange
        return base + jitter
    }

    fun shouldRetryBle(failureCode: String): Boolean {
        return when (failureCode) {
            RatspeakBleGatt.FAILURE_BLUETOOTH_OFF,
            RatspeakBleGatt.FAILURE_PERMISSION,
            RatspeakBleGatt.FAILURE_CONNECT -> true
            RatspeakBleGatt.FAILURE_PAIRING_REQUIRED,
            RatspeakBleGatt.FAILURE_BOND_TIMEOUT,
            RatspeakBleGatt.FAILURE_STALE_BOND -> false
            else -> false
        }
    }

    fun shouldAutoResumeBle(failureCode: String, enabled: Boolean): Boolean {
        return enabled && shouldRetryBle(failureCode)
    }

    data class UsbIdentity(
        val vendorId: Int,
        val productId: Int,
        val serial: String?,
    )

    data class UsbPermissionPlan(
        val candidateIndex: Int?,
        val errorCode: String?,
    )

    /** An empty inventory still emits one sentinel snapshot to clear Rust state. */
    fun usbSnapshotSeed(current: List<UsbIdentity>): List<UsbIdentity?> {
        return if (current.isEmpty()) listOf(null) else current
    }

    fun reduceUsbInventory(
        previous: Set<UsbIdentity>,
        seed: List<UsbIdentity?>,
    ): Set<UsbIdentity> {
        if (seed.any { it == null }) return emptySet()
        return if (seed.isEmpty()) previous else seed.filterNotNull().toSet()
    }

    /**
     * Choose a current USB device without using its transient Android path.
     * A lone serial-unreadable candidate may be provisionally selected and
     * must be revalidated after permission; multiple unknown candidates are
     * never guessed between.
     */
    fun usbPermissionPlan(
        wanted: UsbIdentity,
        candidates: List<UsbIdentity>,
    ): UsbPermissionPlan {
        val matchingIds = candidates.withIndex().filter { (_, candidate) ->
            candidate.vendorId == wanted.vendorId && candidate.productId == wanted.productId
        }
        if (matchingIds.isEmpty()) return UsbPermissionPlan(null, "no_match")
        val wantedSerial = wanted.serial?.trim()?.takeIf { it.isNotEmpty() }
        if (wantedSerial == null) {
            return if (matchingIds.size == 1) {
                UsbPermissionPlan(matchingIds.single().index, null)
            } else {
                UsbPermissionPlan(null, "ambiguous")
            }
        }
        val unknown = matchingIds.filter { (_, candidate) -> candidate.serial == null }
        if (unknown.isNotEmpty()) {
            return if (matchingIds.size == 1) {
                UsbPermissionPlan(matchingIds.single().index, null)
            } else {
                UsbPermissionPlan(null, "ambiguous")
            }
        }
        val exact = matchingIds.filter { (_, candidate) -> candidate.serial?.trim() == wantedSerial }
        return when (exact.size) {
            1 -> UsbPermissionPlan(exact.single().index, null)
            0 -> UsbPermissionPlan(null, "no_match")
            else -> UsbPermissionPlan(null, "ambiguous")
        }
    }

    fun validCallSessionToken(token: String): Boolean {
        return token.length in 16..128 && token.all { character ->
            character.isLetterOrDigit() || character == '-' || character == '_'
        }
    }

    fun callSessionOwns(current: String?, candidate: String): Boolean {
        return current != null && current == candidate
    }

    enum class ServiceReadinessPlan {
        READY,
        START_AND_WAIT,
        START_WITHOUT_WAIT,
    }

    /** Never block Android's main looper while waiting for Service.onCreate(). */
    fun serviceReadinessPlan(
        serviceReady: Boolean,
        callerIsMainThread: Boolean,
    ): ServiceReadinessPlan {
        if (serviceReady) return ServiceReadinessPlan.READY
        return if (callerIsMainThread) {
            ServiceReadinessPlan.START_WITHOUT_WAIT
        } else {
            ServiceReadinessPlan.START_AND_WAIT
        }
    }

    /** A delayed audio-focus callback may act only on its exact current lease. */
    fun voiceMemoInterruptionOwns(
        currentOwner: String?,
        callbackOwner: String,
        callbackListenerIsCurrent: Boolean,
    ): Boolean {
        return callbackListenerIsCurrent && callSessionOwns(currentOwner, callbackOwner)
    }

    enum class MicrophoneCapturePlan {
        PROMOTE,
        ALREADY_ACTIVE,
        DEMOTE,
        ALREADY_INACTIVE,
        REJECT,
    }

    fun microphoneCapturePlan(
        currentOwner: String?,
        candidate: String,
        activate: Boolean,
    ): MicrophoneCapturePlan {
        return if (activate) {
            when (currentOwner) {
                null -> MicrophoneCapturePlan.PROMOTE
                candidate -> MicrophoneCapturePlan.ALREADY_ACTIVE
                else -> MicrophoneCapturePlan.REJECT
            }
        } else {
            when (currentOwner) {
                null -> MicrophoneCapturePlan.ALREADY_INACTIVE
                candidate -> MicrophoneCapturePlan.DEMOTE
                else -> MicrophoneCapturePlan.REJECT
            }
        }
    }

    enum class CapturePromotionPlan {
        PROMOTE,
        ALREADY_PROMOTED,
        REJECT,
    }

    fun capturePromotionPlan(
        routeOwner: String?,
        captureOwner: String?,
        candidate: String,
    ): CapturePromotionPlan {
        if (!callSessionOwns(routeOwner, candidate)) return CapturePromotionPlan.REJECT
        return when (captureOwner) {
            candidate -> CapturePromotionPlan.ALREADY_PROMOTED
            null -> CapturePromotionPlan.PROMOTE
            else -> CapturePromotionPlan.REJECT
        }
    }

    /** Failed demotion retains exact ownership so a stale/new session cannot clear the FGS. */
    fun captureOwnerAfterDemotion(
        captureOwner: String?,
        candidate: String,
        transitionSucceeded: Boolean,
    ): String? {
        return if (transitionSucceeded && callSessionOwns(captureOwner, candidate)) {
            null
        } else {
            captureOwner
        }
    }

    data class NativeOwner(val token: String, val generation: Long)

    data class BleOperation(
        val token: String,
        val generation: Long,
        val address: String,
        val localPort: Int,
    )

    enum class BleReplacementPlan {
        INSTALL,
        IDEMPOTENT,
        BIND_THEN_REPLACE,
        DISPLACE_THEN_BIND,
    }

    fun nativeOwnerMatches(current: NativeOwner?, token: String, generation: Long): Boolean {
        return current?.token == token && current.generation == generation
    }

    /**
     * Pure ownership reducer for BLE install/replace admission. Generation is
     * part of identity so an ABA callback can never become idempotent merely
     * because an operation token or port was reused.
     */
    fun bleReplacementPlan(
        current: BleOperation?,
        replacement: BleOperation,
    ): BleReplacementPlan {
        if (current == null) return BleReplacementPlan.INSTALL
        if (current.token == replacement.token &&
            current.generation == replacement.generation &&
            current.address.equals(replacement.address, ignoreCase = true) &&
            current.localPort == replacement.localPort
        ) {
            return BleReplacementPlan.IDEMPOTENT
        }
        return if (current.localPort == replacement.localPort) {
            BleReplacementPlan.DISPLACE_THEN_BIND
        } else {
            BleReplacementPlan.BIND_THEN_REPLACE
        }
    }

    /** Closed state emitted to Settings, independent of Android API level. */
    fun notificationAuthorizationState(
        runtimePermissionRequired: Boolean,
        runtimePermissionGranted: Boolean,
        notificationsEnabled: Boolean,
        messageChannelEnabled: Boolean,
    ): String {
        if (runtimePermissionRequired && !runtimePermissionGranted) return "denied"
        if (!notificationsEnabled || !messageChannelEnabled) return "denied"
        return "granted"
    }

    fun batteryOptimizationState(exempt: Boolean): String = if (exempt) "exempt" else "not_exempt"

    /** Closed transport classification; precedence mirrors Android's default-network observer. */
    fun networkType(
        available: Boolean,
        wifi: Boolean,
        cellular: Boolean,
        ethernet: Boolean,
    ): String {
        return when {
            !available -> "none"
            wifi -> "wifi"
            cellular -> "cellular"
            ethernet -> "ethernet"
            else -> "unknown"
        }
    }

    /** Match a persisted selector without ever guessing between identical radios. */
    fun uniqueUsbMatch(
        wanted: UsbIdentity,
        candidates: List<Pair<String, UsbIdentity>>,
    ): String? {
        val wantedSerial = wanted.serial?.trim()?.takeIf { it.isNotEmpty() }
        val matches = candidates.filter { (_, candidate) ->
            if (candidate.vendorId != wanted.vendorId || candidate.productId != wanted.productId) {
                false
            } else if (wantedSerial == null) {
                true
            } else {
                candidate.serial?.trim() == wantedSerial
            }
        }
        return matches.singleOrNull()?.first
    }
}
