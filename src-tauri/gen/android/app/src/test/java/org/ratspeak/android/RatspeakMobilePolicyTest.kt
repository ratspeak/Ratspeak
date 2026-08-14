package org.ratspeak.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.net.Socket
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

class RatspeakMobilePolicyTest {
    @Test
    fun mtuUsesTwentyUntilNegotiationActuallySucceeds() {
        assertEquals(20, RatspeakMobilePolicy.attPayload(517, false))
        assertEquals(20, RatspeakMobilePolicy.attPayload(23, true))
        assertEquals(244, RatspeakMobilePolicy.attPayload(247, true))
        assertEquals(514, RatspeakMobilePolicy.attPayload(517, true))
    }

    @Test
    fun gattNotificationNeverQueuesPastOutstandingCallback() {
        assertTrue(RatspeakMobilePolicy.mayQueueGattNotification(true))
        assertFalse(RatspeakMobilePolicy.mayQueueGattNotification(false))
    }

    @Test
    fun reconnectIsFastThenBoundedLowDuty() {
        val delays = (0..12).map { RatspeakMobilePolicy.bleReconnectDelayMs(it, 7) }
        assertTrue(delays.zipWithNext().all { (a, b) -> b >= a })
        assertTrue(delays.first() in 2_000L..2_200L)
        assertTrue(delays.last() in 900_000L..990_000L)
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
