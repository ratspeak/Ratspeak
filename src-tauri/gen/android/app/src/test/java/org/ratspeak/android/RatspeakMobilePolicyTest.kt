package org.ratspeak.android

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.ByteArrayOutputStream
import java.io.IOException
import java.io.OutputStream
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.net.Socket
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

class RatspeakMobilePolicyTest {
    @Test
    fun voiceMemoStartupUsesCapacityBeforeApi31AndAcceptedThresholdAfterward() {
        assertEquals(10_560, RatspeakMobilePolicy.voiceMemoStartupFrames(30, 10_560, 1_920))
        assertEquals(1_920, RatspeakMobilePolicy.voiceMemoStartupFrames(31, 10_560, 1_920))
        assertEquals(10_560, RatspeakMobilePolicy.voiceMemoStartupFrames(31, 10_560, 20_000))
        assertEquals(1, RatspeakMobilePolicy.voiceMemoStartupFrames(31, 0, 0))

        assertEquals(1_920L, RatspeakMobilePolicy.voiceMemoStartupPaddingFrames(10_560, 8_640))
        assertEquals(0L, RatspeakMobilePolicy.voiceMemoStartupPaddingFrames(10_560, 11_520))
        assertEquals(10_560L, RatspeakMobilePolicy.voiceMemoStartupPaddingFrames(10_560, -1))
    }

    @Test
    fun bleProductStateAbiSeparatesGattFromProtocolReadiness() {
        assertEquals(0, RatspeakNativeBridge.BLE_CONNECTING)
        assertEquals(1, RatspeakNativeBridge.BLE_WAITING_FOR_RADIO)
        assertEquals(2, RatspeakNativeBridge.BLE_LISTENER_READY)
        assertEquals(3, RatspeakNativeBridge.BLE_INITIALIZING)
        assertEquals(4, RatspeakNativeBridge.BLE_FAILED)
        assertEquals(5, RatspeakNativeBridge.BLE_DISABLED)
    }

    @Test
    fun mtuUsesTwentyUntilNegotiationActuallySucceeds() {
        assertEquals(20, RatspeakMobilePolicy.attPayload(517, false))
        assertEquals(20, RatspeakMobilePolicy.attPayload(23, true))
        assertEquals(244, RatspeakMobilePolicy.attPayload(247, true))
        assertEquals(514, RatspeakMobilePolicy.attPayload(517, true))
    }

    @Test
    fun rnodeWritesRequireAcknowledgedGattProperty() {
        assertEquals(
            RatspeakMobilePolicy.RnodeWriteMode.WITH_RESPONSE,
            RatspeakMobilePolicy.rnodeWriteMode(hasWrite = true),
        )
        assertEquals(
            "WRITE_NO_RESPONSE-only RNodes fail closed because enqueue is not completion",
            RatspeakMobilePolicy.RnodeWriteMode.UNSUPPORTED,
            RatspeakMobilePolicy.rnodeWriteMode(hasWrite = false),
        )
    }

    @Test
    fun nativeBridgeWorstCaseWriteBoundIncludesDefaultAttPayload() {
        val escapedFrameBytes = 508 * 2 + 3
        val chunks = (escapedFrameBytes + RatspeakMobilePolicy.DEFAULT_ATT_PAYLOAD - 1) /
            RatspeakMobilePolicy.DEFAULT_ATT_PAYLOAD
        val writeBoundMs = chunks * (
            RatspeakMobilePolicy.RNODE_GATT_ENQUEUE_TIMEOUT_MS +
                RatspeakMobilePolicy.RNODE_GATT_CALLBACK_TIMEOUT_MS
            ) + (chunks - 1) * RatspeakMobilePolicy.RNODE_GATT_WRITE_PACING_MS
        assertEquals(51, chunks)
        assertEquals(316_800L, writeBoundMs)
    }

    @Test
    fun nativeBridgeOneByteTcpReadsStillUseAtMostFiftyOneGattWrites() {
        val payload = ByteArray(508) { if (it % 2 == 0) 0xC0.toByte() else 0xDB.toByte() }
        val wire = kissDataFrame(payload)
        assertEquals(RatspeakMobilePolicy.RNODE_MAX_ESCAPED_FRAME_BYTES, wire.size)
        val coalescer = RatspeakMobilePolicy.NativeBridgeOutboundKissCoalescer(
            chunkBytes = RatspeakMobilePolicy.DEFAULT_ATT_PAYLOAD,
        )
        val chunks = wire.flatMap { byte ->
            val offered = coalescer.offer(byteArrayOf(byte))
            assertFalse(offered.overflow)
            offered.chunks
        }

        assertEquals(51, chunks.size)
        assertTrue(chunks.dropLast(1).all { it.wire.size == 20 })
        assertEquals(19, chunks.last().wire.size)
        assertEquals(1, chunks.sumOf { it.completedDataFrames })
        assertArrayEquals(wire, chunks.flatMap { it.wire.asIterable() }.toByteArray())
    }

    @Test
    fun nativeBridgeInboundAssemblerKeepsSplitRecordAcrossSocketReplacement() {
        val assembler = RatspeakMobilePolicy.NativeBridgeInboundKissAssembler()
        val wire = kissDataFrame(byteArrayOf(0x41, 0xC0.toByte(), 0x42))
        val split = wire.size / 2
        val beforeReplacement = assembler.offer(wire, 0, split)
        assertFalse(beforeReplacement.overflow)
        assertTrue(beforeReplacement.records.isEmpty())

        // TCP socket ownership changes here; the assembler is deliberately
        // physical-GATT-generation state and is not reset.
        val afterReplacement = assembler.offer(wire, split, wire.size)
        assertFalse(afterReplacement.overflow)
        assertEquals(1, afterReplacement.records.size)
        assertArrayEquals(wire, afterReplacement.records.single().wire)
        assertEquals(0, afterReplacement.records.single().command)
    }

    @Test
    fun nativeBridgeWriterReplaysWholeInboundRecordAfterPartialWrite() {
        val failed = CountDownLatch(1)
        val delivered = CountDownLatch(1)
        val firstOutput = object : OutputStream() {
            val prefix = ByteArrayOutputStream()
            override fun write(value: Int) = throw IOException("single-byte write not expected")
            override fun write(bytes: ByteArray, offset: Int, length: Int) {
                prefix.write(bytes, offset, minOf(3, length))
                failed.countDown()
                throw IOException("partial socket write")
            }
        }
        val secondBytes = ByteArrayOutputStream()
        val secondOutput = object : OutputStream() {
            override fun write(value: Int) { secondBytes.write(value) }
            override fun write(bytes: ByteArray, offset: Int, length: Int) {
                secondBytes.write(bytes, offset, length)
                delivered.countDown()
            }
        }
        data class TestSocket(val output: OutputStream)
        val writer = RatspeakMobilePolicy.NativeBridgeTcpWriter<TestSocket>(
            maxInboundRecords = 4,
            maxInboundBytes = 1024,
            maxAckRecords = 2,
            closeSocket = {},
            threadName = "partial-write-test",
        )
        val record = kissDataFrame("complete-record".encodeToByteArray())
        try {
            val first = TestSocket(firstOutput)
            assertTrue(writer.install(first, first.output))
            assertTrue(writer.enqueueInbound(record))
            assertTrue(failed.await(1, TimeUnit.SECONDS))

            val second = TestSocket(secondOutput)
            assertTrue(writer.install(second, second.output))
            assertTrue(delivered.await(1, TimeUnit.SECONDS))
            assertArrayEquals(record, secondBytes.toByteArray())
            assertTrue(waitUntil { writer.queuedInboundRecords() == 0 })
        } finally {
            writer.shutdown()
        }
    }

    @Test
    fun nativeBridgeWriterReplacementReplaysDataButDropsStaleControlAndAck() {
        class BlockingOutput : OutputStream() {
            val entered = CountDownLatch(1)
            val released = CountDownLatch(1)
            override fun write(value: Int) = throw IOException("single-byte write not expected")
            override fun write(bytes: ByteArray, offset: Int, length: Int) {
                entered.countDown()
                released.await(2, TimeUnit.SECONDS)
                throw IOException("closed while blocked")
            }
            override fun close() { released.countDown() }
        }
        data class TestSocket(val output: OutputStream)
        val firstOutput = BlockingOutput()
        val delivered = CountDownLatch(1)
        val secondBytes = ByteArrayOutputStream()
        val secondOutput = object : OutputStream() {
            override fun write(value: Int) { secondBytes.write(value) }
            override fun write(bytes: ByteArray, offset: Int, length: Int) {
                secondBytes.write(bytes, offset, length)
                delivered.countDown()
            }
        }
        val writer = RatspeakMobilePolicy.NativeBridgeTcpWriter<TestSocket>(
            maxInboundRecords = 4,
            maxInboundBytes = 1024,
            maxAckRecords = 2,
            closeSocket = { it.output.close() },
            threadName = "blocked-write-test",
        )
        val record = kissDataFrame("blocked-record".encodeToByteArray())
        try {
            val first = TestSocket(firstOutput)
            assertTrue(writer.install(first, first.output))
            assertTrue(writer.enqueueInbound(record))
            assertTrue(firstOutput.entered.await(1, TimeUnit.SECONDS))
            assertTrue(
                writer.enqueueControl(
                    byteArrayOf(0xC0.toByte(), 0x06, 0x01, 0xC0.toByte()),
                ),
            )
            assertTrue(
                writer.enqueueAck(first, RatspeakMobilePolicy.nativeBridgeAckFrame(1)),
            )

            val second = TestSocket(secondOutput)
            assertTrue(writer.install(second, second.output))
            assertTrue(delivered.await(1, TimeUnit.SECONDS))
            assertTrue(waitUntil { writer.queuedControlRecords() == 0 })
            assertTrue(waitUntil { writer.queuedAckRecords() == 0 })
            // Neither the old generation's Ready-like control nor its ACK may
            // be replayed onto the replacement socket.
            assertArrayEquals(record, secondBytes.toByteArray())
        } finally {
            writer.shutdown()
        }
    }

    @Test
    fun nativeBridgeAckIsClosedKissAndEscapesCounterBytes() {
        assertArrayEquals(
            byteArrayOf(
                0xC0.toByte(), 0xA0.toByte(),
                0x52, 0x53, 0x42, 0x41, 0x01,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
                0xC0.toByte(),
            ),
            RatspeakMobilePolicy.nativeBridgeAckFrame(1),
        )

        val counter = 0x000000000000C0DBL
        val frame = RatspeakMobilePolicy.nativeBridgeAckFrame(counter)
        assertEquals(0xC0.toByte(), frame.first())
        assertEquals(RatspeakMobilePolicy.NATIVE_BRIDGE_ACK_COMMAND.toByte(), frame[1])
        assertEquals(0xC0.toByte(), frame.last())
        assertTrue(
            frame.toList().windowed(2).any { it == listOf(0xDB.toByte(), 0xDC.toByte()) },
        )
        assertTrue(
            frame.toList().windowed(2).any { it == listOf(0xDB.toByte(), 0xDD.toByte()) },
        )
    }

    @Test
    fun nativeBridgeInboundBoundsAndWriterQueueFailClosed() {
        val assembler = RatspeakMobilePolicy.NativeBridgeInboundKissAssembler(maxFrameBytes = 5)
        val oversized = assembler.offer(
            byteArrayOf(
                0xC0.toByte(), 0x00, 0x01, 0x02, 0x03, 0xC0.toByte(),
            ),
        )
        assertTrue(oversized.overflow)

        data class TestSocket(val output: OutputStream)
        val writer = RatspeakMobilePolicy.NativeBridgeTcpWriter<TestSocket>(
            maxInboundRecords = 1,
            maxInboundBytes = 4,
            maxAckRecords = 1,
            closeSocket = {},
            threadName = "bridge-bound-test",
        )
        try {
            assertTrue(
                "control without an active socket is an intentional discard",
                writer.enqueueControl(
                    byteArrayOf(0xC0.toByte(), 0x06, 0x01, 0xC0.toByte()),
                ),
            )
            assertEquals(0, writer.queuedControlRecords())
            assertTrue(writer.enqueueInbound(byteArrayOf(1, 2, 3, 4)))
            assertFalse(writer.enqueueInbound(byteArrayOf(5)))
        } finally {
            writer.shutdown()
        }
    }

    @Test
    fun controlQuiescenceWaitsAfterCloseAndCommitsInstallOnlyAfterFence() {
        val millisecond = 1_000_000L
        var now = 0L
        var installed = false
        val gate = RatspeakMobilePolicy.NativeBridgeControlQuiescence(
            quietIntervalNanos = 200 * millisecond,
            hardMaxNanos = 2_000 * millisecond,
            nowNanos = { now },
            waitNanos = { delay ->
                assertFalse("socket N+1 installed before quiet boundary", installed)
                now += delay
            },
        )
        val boundary = gate.beginAfterClosure()
        val outcome = boundary.awaitQuietAndCommit(
            shouldContinue = { true },
            action = {
                installed = true
                true
            },
        )
        assertEquals(RatspeakMobilePolicy.NativeBridgeControlQuiescence.Outcome.READY, outcome)
        assertTrue(installed)
        assertEquals(200 * millisecond, now)
    }

    @Test
    fun lateControlExtendsQuiescenceButDataDoesNot() {
        val millisecond = 1_000_000L

        var controlNow = 0L
        var injectControl = true
        lateinit var controlGate: RatspeakMobilePolicy.NativeBridgeControlQuiescence
        controlGate = RatspeakMobilePolicy.NativeBridgeControlQuiescence(
            quietIntervalNanos = 200 * millisecond,
            hardMaxNanos = 2_000 * millisecond,
            nowNanos = { controlNow },
            waitNanos = { delay ->
                val step = minOf(delay, 100 * millisecond)
                controlNow += step
                if (injectControl) {
                    injectControl = false
                    controlGate.observeCompleteRecord(isData = false)
                }
            },
        )
        assertEquals(
            RatspeakMobilePolicy.NativeBridgeControlQuiescence.Outcome.READY,
            controlGate.beginAfterClosure().awaitQuiet { true },
        )
        assertEquals(300 * millisecond, controlNow)

        var dataNow = 0L
        var injectData = true
        lateinit var dataGate: RatspeakMobilePolicy.NativeBridgeControlQuiescence
        dataGate = RatspeakMobilePolicy.NativeBridgeControlQuiescence(
            quietIntervalNanos = 200 * millisecond,
            hardMaxNanos = 2_000 * millisecond,
            nowNanos = { dataNow },
            waitNanos = { delay ->
                val step = minOf(delay, 100 * millisecond)
                dataNow += step
                if (injectData) {
                    injectData = false
                    dataGate.observeCompleteRecord(isData = true)
                }
            },
        )
        assertEquals(
            RatspeakMobilePolicy.NativeBridgeControlQuiescence.Outcome.READY,
            dataGate.beginAfterClosure().awaitQuiet { true },
        )
        assertEquals(200 * millisecond, dataNow)
    }

    @Test
    fun endlessControlsHitQuiescenceHardFailure() {
        val millisecond = 1_000_000L
        var now = 0L
        lateinit var gate: RatspeakMobilePolicy.NativeBridgeControlQuiescence
        gate = RatspeakMobilePolicy.NativeBridgeControlQuiescence(
            quietIntervalNanos = 200 * millisecond,
            hardMaxNanos = 2_000 * millisecond,
            nowNanos = { now },
            waitNanos = { delay ->
                now += minOf(delay, 50 * millisecond)
                gate.observeCompleteRecord(isData = false)
            },
        )
        assertEquals(
            RatspeakMobilePolicy.NativeBridgeControlQuiescence.Outcome.HARD_FAILURE,
            gate.beginAfterClosure().awaitQuiet { true },
        )
        assertEquals(2_000 * millisecond, now)
    }

    private fun kissDataFrame(payload: ByteArray): ByteArray {
        val framed = ByteArrayOutputStream(payload.size * 2 + 3)
        framed.write(0xC0)
        framed.write(0x00)
        payload.forEach { byte ->
            when (byte.toInt() and 0xFF) {
                0xC0 -> { framed.write(0xDB); framed.write(0xDC) }
                0xDB -> { framed.write(0xDB); framed.write(0xDD) }
                else -> framed.write(byte.toInt() and 0xFF)
            }
        }
        framed.write(0xC0)
        return framed.toByteArray()
    }

    private fun waitUntil(timeoutMs: Long = 1_000, condition: () -> Boolean): Boolean {
        val deadline = System.nanoTime() + TimeUnit.MILLISECONDS.toNanos(timeoutMs)
        while (System.nanoTime() < deadline) {
            if (condition()) return true
            Thread.sleep(5)
        }
        return condition()
    }

    @Test
    fun gattNotificationNeverQueuesPastOutstandingCallback() {
        assertTrue(RatspeakMobilePolicy.mayQueueGattNotification(true))
        assertFalse(RatspeakMobilePolicy.mayQueueGattNotification(false))
    }

    @Test
    fun reconnectIsFastThenBoundedLowDuty() {
        val delays = (0..24).map { RatspeakMobilePolicy.bleReconnectDelayMs(it, 7) }
        assertTrue(delays.zipWithNext().all { (a, b) -> b >= a })
        assertTrue(delays.first() in 2_000L..2_200L)
        assertTrue(delays[5] in 60_000L..66_000L)
        assertTrue(delays[12] in 60_000L..66_000L)
        assertTrue(delays[13] in 120_000L..132_000L)
        assertTrue(delays[17] in 120_000L..132_000L)
        assertTrue(delays[18] in 600_000L..660_000L)
        assertTrue(delays.last() in 600_000L..660_000L)
    }

    @Test
    fun pairingFailuresRequireUserAction() {
        assertFalse(RatspeakMobilePolicy.shouldRetryBle(RatspeakBleGatt.FAILURE_PAIRING_REQUIRED))
        assertFalse(RatspeakMobilePolicy.shouldRetryBle(RatspeakBleGatt.FAILURE_STALE_BOND))
        assertTrue(RatspeakMobilePolicy.shouldRetryBle(RatspeakBleGatt.FAILURE_CONNECT))
        // Android GATT 133 is normalized to the closed transient code before
        // policy; arbitrary/raw text can never alter retry behavior.
        assertFalse(RatspeakMobilePolicy.shouldRetryBle("gatt_133"))
    }

    @Test
    fun androidBleAutoResumeIsExplicitPolicyOverRetryableLoss() {
        assertTrue(
            RatspeakMobilePolicy.shouldAutoResumeBle(
                RatspeakBleGatt.FAILURE_BLUETOOTH_OFF,
                true,
            ),
        )
        assertFalse(
            RatspeakMobilePolicy.shouldAutoResumeBle(
                RatspeakBleGatt.FAILURE_BLUETOOTH_OFF,
                false,
            ),
        )
        assertFalse(
            RatspeakMobilePolicy.shouldAutoResumeBle(
                RatspeakBleGatt.FAILURE_STALE_BOND,
                true,
            ),
        )
    }

    @Test
    fun usbSelectionNeverGuessesBetweenIdenticalDevices() {
        val wanted = RatspeakMobilePolicy.UsbIdentity(0x10c4, 0xea60, null)
        val one = listOf("/dev/a" to wanted)
        assertEquals("/dev/a", RatspeakMobilePolicy.uniqueUsbMatch(wanted, one))
        assertNull(RatspeakMobilePolicy.uniqueUsbMatch(wanted, one + ("/dev/b" to wanted)))

        val serialWanted = wanted.copy(serial = "radio-2")
        val candidates = listOf(
            "/dev/a" to wanted.copy(serial = "radio-1"),
            "/dev/b" to wanted.copy(serial = "radio-2"),
        )
        assertEquals("/dev/b", RatspeakMobilePolicy.uniqueUsbMatch(serialWanted, candidates))
    }

    @Test
    fun usbPermissionSelectorUsesStableIdentityAndFailsClosedOnAmbiguity() {
        val wanted = RatspeakMobilePolicy.UsbIdentity(0x10c4, 0xea60, "radio-2")
        assertEquals(
            0,
            RatspeakMobilePolicy.usbPermissionPlan(
                wanted,
                listOf(wanted.copy(serial = null)),
            ).candidateIndex,
        )
        assertEquals(
            "ambiguous",
            RatspeakMobilePolicy.usbPermissionPlan(
                wanted,
                listOf(wanted.copy(serial = null), wanted.copy(serial = null)),
            ).errorCode,
        )
        assertEquals(
            1,
            RatspeakMobilePolicy.usbPermissionPlan(
                wanted,
                listOf(wanted.copy(serial = "radio-1"), wanted),
            ).candidateIndex,
        )
        assertEquals(
            "no_match",
            RatspeakMobilePolicy.usbPermissionPlan(
                wanted,
                listOf(wanted.copy(serial = "radio-1")),
            ).errorCode,
        )
        val withoutSerial = wanted.copy(serial = null)
        assertEquals(
            "ambiguous",
            RatspeakMobilePolicy.usbPermissionPlan(
                withoutSerial,
                listOf(withoutSerial, withoutSerial),
            ).errorCode,
        )
    }

    @Test
    fun emptyUsbSeedClearsPreviouslyAttachedInventory() {
        val attached = RatspeakMobilePolicy.UsbIdentity(0x10c4, 0xea60, "radio-1")
        val emptySeed = RatspeakMobilePolicy.usbSnapshotSeed(emptyList())
        assertEquals(listOf<Any?>(null), emptySeed)
        assertTrue(
            RatspeakMobilePolicy.reduceUsbInventory(setOf(attached), emptySeed).isEmpty(),
        )
    }

    @Test
    fun bleListenerDiscardsMultipleClosedClientsBeforeLiveGeneration() {
        ServerSocket().use { listener ->
            listener.reuseAddress = true
            listener.bind(
                InetSocketAddress(InetAddress.getByName("127.0.0.1"), 0),
                1,
            )
            val releaseLive = CountDownLatch(1)
            val connectorDone = CountDownLatch(1)
            val connector = Thread({
                try {
                    fun connect(): Socket = Socket().apply {
                        connect(InetSocketAddress("127.0.0.1", listener.localPort), 1_000)
                    }
                    connect().close()
                    connect().close()
                    connect().use { live ->
                        live.getOutputStream().write(0x42)
                        live.getOutputStream().flush()
                        releaseLive.await(3, TimeUnit.SECONDS)
                    }
                } finally {
                    connectorDone.countDown()
                }
            }, "ble-listener-test-connector").apply { start() }
            val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(4)
            val accepted = RatspeakBleSocketQueue.acceptUsable(
                listener,
                shouldContinue = { System.nanoTime() < deadline },
                staleProbeMs = 1_000,
            ) ?: throw AssertionError("expected bounded live client")
            accepted.socket.use {
                assertEquals(0x42, accepted.input.read())
            }
            releaseLive.countDown()
            assertTrue(connectorDone.await(3, TimeUnit.SECONDS))
            connector.join(100)
        }
    }

    @Test
    fun callSessionStopRequiresTheExactCurrentOwner() {
        val sessionA = "call_session_A_0001"
        val sessionB = "call_session_B_0002"
        assertTrue(RatspeakMobilePolicy.validCallSessionToken(sessionA))
        assertFalse(RatspeakMobilePolicy.validCallSessionToken("short"))
        assertFalse(RatspeakMobilePolicy.validCallSessionToken("call session with spaces"))
        assertTrue(RatspeakMobilePolicy.callSessionOwns(sessionA, sessionA))
        // A stale route request from A cannot update the replacement B.
        assertFalse(RatspeakMobilePolicy.callSessionOwns(sessionB, sessionA))
        assertTrue(RatspeakMobilePolicy.callSessionOwns(sessionB, sessionB))
        assertFalse(RatspeakMobilePolicy.callSessionOwns(null, sessionA))
        assertEquals(
            RatspeakMobilePolicy.CapturePromotionPlan.PROMOTE,
            RatspeakMobilePolicy.capturePromotionPlan(sessionA, null, sessionA),
        )
        assertEquals(
            RatspeakMobilePolicy.CapturePromotionPlan.ALREADY_PROMOTED,
            RatspeakMobilePolicy.capturePromotionPlan(sessionA, sessionA, sessionA),
        )
        // A denied promotion leaves capture unowned, while a stale callback
        // from A can neither promote nor demote the replacement B.
        assertEquals(
            RatspeakMobilePolicy.CapturePromotionPlan.REJECT,
            RatspeakMobilePolicy.capturePromotionPlan(sessionB, null, sessionA),
        )
        assertEquals(
            RatspeakMobilePolicy.CapturePromotionPlan.REJECT,
            RatspeakMobilePolicy.capturePromotionPlan(sessionB, sessionB, sessionA),
        )
        assertEquals(
            sessionA,
            RatspeakMobilePolicy.captureOwnerAfterDemotion(sessionA, sessionA, false),
        )
        assertNull(
            RatspeakMobilePolicy.captureOwnerAfterDemotion(sessionA, sessionA, true),
        )
        assertEquals(
            sessionB,
            RatspeakMobilePolicy.captureOwnerAfterDemotion(sessionB, sessionA, true),
        )
        // Activity destruction cancels only its exact interactive prime and
        // cannot tear down an established Rust-owned call session.
        assertFalse(RatspeakMobilePolicy.callSessionOwns(sessionA, "interactive_route_0001"))
        assertTrue(
            RatspeakMobilePolicy.callSessionOwns(
                "interactive_route_0001",
                "interactive_route_0001",
            ),
        )
    }

    @Test
    fun microphoneCaptureLeaseRejectsCrossSessionPromotionAndCleanup() {
        val call = "call_session_A_0001"
        val memo = "vmr-0000000000000001"
        assertEquals(
            RatspeakMobilePolicy.MicrophoneCapturePlan.PROMOTE,
            RatspeakMobilePolicy.microphoneCapturePlan(null, memo, true),
        )
        assertEquals(
            RatspeakMobilePolicy.MicrophoneCapturePlan.ALREADY_ACTIVE,
            RatspeakMobilePolicy.microphoneCapturePlan(memo, memo, true),
        )
        assertEquals(
            RatspeakMobilePolicy.MicrophoneCapturePlan.REJECT,
            RatspeakMobilePolicy.microphoneCapturePlan(memo, call, true),
        )
        assertEquals(
            RatspeakMobilePolicy.MicrophoneCapturePlan.REJECT,
            RatspeakMobilePolicy.microphoneCapturePlan(memo, call, false),
        )
        assertEquals(
            RatspeakMobilePolicy.MicrophoneCapturePlan.DEMOTE,
            RatspeakMobilePolicy.microphoneCapturePlan(memo, memo, false),
        )
    }

    @Test
    fun serviceReadinessWaitsOnlyOffTheMainLooper() {
        assertEquals(
            RatspeakMobilePolicy.ServiceReadinessPlan.READY,
            RatspeakMobilePolicy.serviceReadinessPlan(
                serviceReady = true,
                callerIsMainThread = true,
            ),
        )
        assertEquals(
            RatspeakMobilePolicy.ServiceReadinessPlan.START_AND_WAIT,
            RatspeakMobilePolicy.serviceReadinessPlan(
                serviceReady = false,
                callerIsMainThread = false,
            ),
        )
        assertEquals(
            RatspeakMobilePolicy.ServiceReadinessPlan.START_WITHOUT_WAIT,
            RatspeakMobilePolicy.serviceReadinessPlan(
                serviceReady = false,
                callerIsMainThread = true,
            ),
        )
    }

    @Test
    fun voiceMemoInterruptionRequiresTheExactOwnerAndListener() {
        val oldSession = "vmr-0000000000000001"
        val replacement = "vmr-0000000000000002"
        assertTrue(
            RatspeakMobilePolicy.voiceMemoInterruptionOwns(
                oldSession,
                oldSession,
                callbackListenerIsCurrent = true,
            ),
        )
        assertFalse(
            RatspeakMobilePolicy.voiceMemoInterruptionOwns(
                replacement,
                oldSession,
                callbackListenerIsCurrent = true,
            ),
        )
        assertFalse(
            RatspeakMobilePolicy.voiceMemoInterruptionOwns(
                oldSession,
                oldSession,
                callbackListenerIsCurrent = false,
            ),
        )
        assertFalse(
            RatspeakMobilePolicy.voiceMemoInterruptionOwns(
                null,
                oldSession,
                callbackListenerIsCurrent = true,
            ),
        )
    }

    @Test
    fun nativeGenerationRejectsStaleTeardown() {
        val current = RatspeakMobilePolicy.NativeOwner("00112233445566778899aabbccddeeff", 42)
        assertTrue(RatspeakMobilePolicy.nativeOwnerMatches(current, current.token, 42))
        assertFalse(RatspeakMobilePolicy.nativeOwnerMatches(current, current.token, 41))
        assertFalse(
            RatspeakMobilePolicy.nativeOwnerMatches(
                current,
                "ffeeddccbbaa99887766554433221100",
                42,
            ),
        )
    }

    @Test
    fun bleOwnerReducerFencesAbaAndPlansTransactionalReplacement() {
        val current = RatspeakMobilePolicy.BleOperation(
            "00112233445566778899aabbccddeeff",
            42,
            "00:11:22:33:44:55",
            31_000,
        )
        assertEquals(
            RatspeakMobilePolicy.BleReplacementPlan.INSTALL,
            RatspeakMobilePolicy.bleReplacementPlan(null, current),
        )
        assertEquals(
            RatspeakMobilePolicy.BleReplacementPlan.IDEMPOTENT,
            RatspeakMobilePolicy.bleReplacementPlan(current, current.copy(address = "00:11:22:33:44:55")),
        )
        assertEquals(
            RatspeakMobilePolicy.BleReplacementPlan.BIND_THEN_REPLACE,
            RatspeakMobilePolicy.bleReplacementPlan(current, current.copy(localPort = 31_001)),
        )
        assertEquals(
            RatspeakMobilePolicy.BleReplacementPlan.DISPLACE_THEN_BIND,
            RatspeakMobilePolicy.bleReplacementPlan(current, current.copy(generation = 43)),
        )
        assertEquals(
            RatspeakMobilePolicy.BleReplacementPlan.DISPLACE_THEN_BIND,
            RatspeakMobilePolicy.bleReplacementPlan(
                current,
                current.copy(token = "ffeeddccbbaa99887766554433221100"),
            ),
        )
    }

    @Test
    fun settingsAndNetworkStatesAreClosedAndDeterministic() {
        assertEquals(
            "denied",
            RatspeakMobilePolicy.notificationAuthorizationState(true, false, true, true),
        )
        assertEquals(
            "denied",
            RatspeakMobilePolicy.notificationAuthorizationState(false, true, false, true),
        )
        assertEquals(
            "denied",
            RatspeakMobilePolicy.notificationAuthorizationState(false, true, true, false),
        )
        assertEquals(
            "granted",
            RatspeakMobilePolicy.notificationAuthorizationState(true, true, true, true),
        )
        assertEquals("exempt", RatspeakMobilePolicy.batteryOptimizationState(true))
        assertEquals("not_exempt", RatspeakMobilePolicy.batteryOptimizationState(false))

        assertEquals("none", RatspeakMobilePolicy.networkType(false, true, true, true))
        assertEquals("wifi", RatspeakMobilePolicy.networkType(true, true, true, true))
        assertEquals("cellular", RatspeakMobilePolicy.networkType(true, false, true, true))
        assertEquals("ethernet", RatspeakMobilePolicy.networkType(true, false, false, true))
        assertEquals("unknown", RatspeakMobilePolicy.networkType(true, false, false, false))
    }

    @Test
    fun attachmentMemoryPressureDoesNotTreatPickerLifecycleAsPressure() {
        assertNull(RatspeakMobilePolicy.attachmentMemoryPressure(5))
        assertEquals(false, RatspeakMobilePolicy.attachmentMemoryPressure(10))
        assertEquals(true, RatspeakMobilePolicy.attachmentMemoryPressure(15))
        assertNull(RatspeakMobilePolicy.attachmentMemoryPressure(20))
        assertEquals(false, RatspeakMobilePolicy.attachmentMemoryPressure(40))
        assertEquals(true, RatspeakMobilePolicy.attachmentMemoryPressure(60))
        assertEquals(true, RatspeakMobilePolicy.attachmentMemoryPressure(80))
    }
}
