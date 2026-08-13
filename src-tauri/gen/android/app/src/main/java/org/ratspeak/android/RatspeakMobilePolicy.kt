package org.ratspeak.android

import kotlin.math.absoluteValue

/** Pure Android-mobile policy helpers kept free of framework dependencies. */
object RatspeakMobilePolicy {
    const val DEFAULT_ATT_PAYLOAD = 20
    private const val MAX_ATT_PAYLOAD = 514

    /** ATT payload is MTU minus the three-byte ATT header. */
    fun attPayload(mtu: Int, negotiationSucceeded: Boolean): Int {
        if (!negotiationSucceeded) return DEFAULT_ATT_PAYLOAD
        return (mtu - 3).coerceIn(1, MAX_ATT_PAYLOAD)
    }

    /**
     * Fast retries handle Android's transient GATT failures; later retries
     * become low duty but never give up on an explicitly configured radio.
     * A stable token-derived jitter prevents several radios reconnecting in
     * lockstep without making tests or user-visible timing nondeterministic.
     */
    fun bleReconnectDelayMs(attempt: Int, stableSeed: Int): Long {
        val delays = longArrayOf(2_000, 5_000, 10_000, 20_000, 30_000, 60_000, 300_000, 900_000)
        val base = delays[attempt.coerceAtLeast(0).coerceAtMost(delays.lastIndex)]
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
