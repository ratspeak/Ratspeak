package org.ratspeak.android

import android.annotation.SuppressLint
import android.content.Context
import android.media.AudioAttributes
import android.media.AudioDeviceInfo
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import androidx.annotation.RequiresApi

/** Process-owned route/focus state for an established LXST call. */
object RatspeakCallAudio {
    private const val INTERACTIVE_OWNER = "interactive_route_0001"
    private val lock = Any()
    private val handler = Handler(Looper.getMainLooper())
    private val focusListener = AudioManager.OnAudioFocusChangeListener { }
    private var focusRequest: AudioFocusRequest? = null
    private var proximityWakeLock: PowerManager.WakeLock? = null
    private var active = false
    private var route = "earpiece"
    private var ownerToken: String? = null
    private var captureOwnerToken: String? = null
    private var captureDemotionRetryToken: String? = null

    @JvmStatic
    fun isActive(): Boolean = synchronized(lock) { active }

    @JvmStatic
    fun primeInteractive(context: Context, role: String): Boolean = synchronized(lock) {
        // Missing session identity is valid only before Rust establishes a
        // call. It can never mutate a later call after an ABA replacement.
        if (active) {
            if (ownerToken != INTERACTIVE_OWNER) return false
            return updateRouteLocked(context.applicationContext, role)
        }
        startForSessionLocked(context, INTERACTIVE_OWNER, role)
    }

    @JvmStatic
    fun updateRouteForSession(context: Context, sessionToken: String, role: String): Boolean =
        synchronized(lock) {
            if (!RatspeakMobilePolicy.validCallSessionToken(sessionToken) ||
                !RatspeakMobilePolicy.callSessionOwns(ownerToken, sessionToken)
            ) return false
            updateRouteLocked(context.applicationContext, role)
        }

    @JvmStatic
    fun startForSession(context: Context, sessionToken: String, initialRoute: String): Boolean =
        synchronized(lock) {
            if (!RatspeakMobilePolicy.validCallSessionToken(sessionToken)) return false
            if (active && ownerToken == sessionToken) {
                return updateRouteLocked(context.applicationContext, initialRoute)
            }
            if (active && ownerToken == INTERACTIVE_OWNER) {
                // Route/focus/proximity are process resources. Transfer them
                // atomically only after the requested route succeeds; capture
                // remains separately fenced and is not active during prime.
                if (!updateRouteLocked(context.applicationContext, initialRoute)) return false
                ownerToken = sessionToken
                return true
            }
            // A delayed start from another Rust session cannot replace the
            // current exact owner. Rust must first stop that owner by token.
            if (active) return false
            startForSessionLocked(context, sessionToken, initialRoute)
        }

    private fun startForSessionLocked(
        context: Context,
        sessionToken: String,
        initialRoute: String,
    ): Boolean {
        val application = context.applicationContext
        val manager = application.getSystemService(AudioManager::class.java)
            ?: return false
        val requestedRoute = if (initialRoute.equals("speaker", ignoreCase = true)) "speaker" else "earpiece"
        val preferEarpiece = requestedRoute != "speaker"
        if (!requestFocus(manager)) {
            rollbackStart(manager)
            return false
        }
        if (!configureRoute(manager, preferEarpiece)) {
            rollbackStart(manager)
            return false
        }
        try {
            syncProximity(application, preferEarpiece)
        } catch (_: Throwable) {
            rollbackStart(manager)
            return false
        }
        route = requestedRoute
        ownerToken = sessionToken
        active = true
        return true
    }

    /** Promote the exact established session before Rust opens microphone capture. */
    @JvmStatic
    fun promoteCaptureForSession(context: Context, sessionToken: String): Boolean =
        synchronized(lock) {
            if (!RatspeakMobilePolicy.validCallSessionToken(sessionToken) ||
                !active
            ) return false
            when (RatspeakMobilePolicy.capturePromotionPlan(
                ownerToken,
                captureOwnerToken,
                sessionToken,
            )) {
                RatspeakMobilePolicy.CapturePromotionPlan.ALREADY_PROMOTED -> return true
                RatspeakMobilePolicy.CapturePromotionPlan.REJECT -> return false
                RatspeakMobilePolicy.CapturePromotionPlan.PROMOTE -> Unit
            }
            if (!RatspeakService.setCallCaptureActive(context.applicationContext, true)) return false
            captureOwnerToken = sessionToken
            true
        }

    /** Demote only the exact capture owner; stale session cleanup is a no-op. */
    @JvmStatic
    fun demoteCaptureForSession(context: Context, sessionToken: String): Boolean =
        synchronized(lock) {
            if (!RatspeakMobilePolicy.validCallSessionToken(sessionToken) ||
                !RatspeakMobilePolicy.callSessionOwns(captureOwnerToken, sessionToken)
            ) return false
            val demoted = RatspeakService.setCallCaptureActive(context.applicationContext, false)
            captureOwnerToken = RatspeakMobilePolicy.captureOwnerAfterDemotion(
                captureOwnerToken,
                sessionToken,
                demoted,
            )
            if (demoted) {
                captureDemotionRetryToken = null
            } else {
                scheduleCaptureDemotion(context.applicationContext, sessionToken)
            }
            demoted
        }

    @JvmStatic
    fun stop(context: Context, waitForNoProximity: Boolean = true) = synchronized(lock) {
        stopLocked(
            context.applicationContext,
            INTERACTIVE_OWNER,
            exact = true,
            waitForNoProximity = waitForNoProximity,
        )
    }

    /** Activity cleanup for a UI-only dial/answer prime; never stops a Rust session. */
    @JvmStatic
    fun cancelInteractivePrime(context: Context): Boolean = synchronized(lock) {
        stopLocked(
            context.applicationContext,
            INTERACTIVE_OWNER,
            exact = true,
            waitForNoProximity = false,
        )
    }

    @JvmStatic
    fun stopForSession(context: Context, sessionToken: String): Boolean = synchronized(lock) {
        if (!RatspeakMobilePolicy.validCallSessionToken(sessionToken)) return false
        stopLocked(context.applicationContext, sessionToken, exact = true)
    }

    private fun stopLocked(
        context: Context,
        candidateToken: String?,
        exact: Boolean,
        waitForNoProximity: Boolean = true,
    ): Boolean {
        if (exact && !RatspeakMobilePolicy.callSessionOwns(ownerToken, candidateToken ?: "")) {
            return false
        }
        val application = context.applicationContext
        if (captureOwnerToken == candidateToken) {
            val demoted = RatspeakService.setCallCaptureActive(application, false)
            captureOwnerToken = RatspeakMobilePolicy.captureOwnerAfterDemotion(
                captureOwnerToken,
                candidateToken ?: "",
                demoted,
            )
            if (demoted) {
                captureDemotionRetryToken = null
            } else if (candidateToken != null) {
                scheduleCaptureDemotion(application, candidateToken)
            }
        }
        active = false
        ownerToken = null
        releaseProximity(waitForNoProximity)
        val manager = application.getSystemService(AudioManager::class.java)
        if (manager != null) {
            @Suppress("DEPRECATION")
            manager.isSpeakerphoneOn = false
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                try { manager.clearCommunicationDevice() } catch (_: Throwable) {}
            }
            manager.mode = AudioManager.MODE_NORMAL
            focusRequest?.let {
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                    try { manager.abandonAudioFocusRequest(it) } catch (_: Throwable) {}
                }
            } ?: run {
                @Suppress("DEPRECATION")
                manager.abandonAudioFocus(focusListener)
            }
        }
        focusRequest = null
        return true
    }

    /**
     * A failed Android FGS downgrade quarantines the exact old capture owner
     * and retries at low duty. A replacement cannot promote until this clears,
     * and an old retry can never demote a later owner.
     */
    private fun scheduleCaptureDemotion(context: Context, sessionToken: String) {
        if (captureDemotionRetryToken == sessionToken) return
        captureDemotionRetryToken = sessionToken
        val application = context.applicationContext
        handler.postDelayed(object : Runnable {
            override fun run() {
                synchronized(lock) {
                    if (!RatspeakMobilePolicy.callSessionOwns(captureOwnerToken, sessionToken)) {
                        if (captureDemotionRetryToken == sessionToken) {
                            captureDemotionRetryToken = null
                        }
                        return
                    }
                    val demoted = RatspeakService.setCallCaptureActive(application, false)
                    captureOwnerToken = RatspeakMobilePolicy.captureOwnerAfterDemotion(
                        captureOwnerToken,
                        sessionToken,
                        demoted,
                    )
                    if (demoted) {
                        captureDemotionRetryToken = null
                    } else {
                        handler.postDelayed(this, 30_000L)
                    }
                }
            }
        }, 2_000L)
    }

    private fun updateRouteLocked(context: Context, requestedRole: String): Boolean {
        if (!active || ownerToken == null) return false
        val manager = context.getSystemService(AudioManager::class.java) ?: return false
        val nextRoute = if (requestedRole.equals("speaker", ignoreCase = true)) "speaker" else "earpiece"
        val preferEarpiece = nextRoute != "speaker"
        if (!configureRoute(manager, preferEarpiece)) return false
        syncProximity(context, preferEarpiece)
        route = nextRoute
        return true
    }

    private fun requestFocus(manager: AudioManager): Boolean {
        if (focusRequest != null) return true
        val attributes = AudioAttributes.Builder()
            .setUsage(AudioAttributes.USAGE_VOICE_COMMUNICATION)
            .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
            .build()
        val result = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val request = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT)
                .setAudioAttributes(attributes)
                .setOnAudioFocusChangeListener(focusListener, handler)
                .build()
            val value = manager.requestAudioFocus(request)
            if (value == AudioManager.AUDIOFOCUS_REQUEST_GRANTED) focusRequest = request
            value
        } else {
            @Suppress("DEPRECATION")
            manager.requestAudioFocus(
                focusListener,
                AudioManager.STREAM_VOICE_CALL,
                AudioManager.AUDIOFOCUS_GAIN_TRANSIENT,
            )
        }
        return result == AudioManager.AUDIOFOCUS_REQUEST_GRANTED
    }

    private fun configureRoute(manager: AudioManager, preferEarpiece: Boolean): Boolean {
        return try {
            manager.mode = AudioManager.MODE_IN_COMMUNICATION
            @Suppress("DEPRECATION")
            manager.isSpeakerphoneOn = !preferEarpiece
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S) return true
            val selected = selectDevice(manager, preferEarpiece)
            if (selected == null) {
                manager.clearCommunicationDevice()
                return true
            }
            val current = manager.communicationDevice
            if (current != null && current.type != selected.type) manager.clearCommunicationDevice()
            if (!manager.setCommunicationDevice(selected)) {
                @Suppress("DEPRECATION")
                manager.isSpeakerphoneOn = !preferEarpiece
            }
            true
        } catch (_: Throwable) {
            false
        }
    }

    private fun rollbackStart(manager: AudioManager) {
        active = false
        ownerToken = null
        releaseProximity(false)
        @Suppress("DEPRECATION")
        try { manager.isSpeakerphoneOn = false } catch (_: Throwable) {}
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            try { manager.clearCommunicationDevice() } catch (_: Throwable) {}
        }
        try { manager.mode = AudioManager.MODE_NORMAL } catch (_: Throwable) {}
        focusRequest?.let {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                try { manager.abandonAudioFocusRequest(it) } catch (_: Throwable) {}
            }
        } ?: run {
            @Suppress("DEPRECATION")
            try { manager.abandonAudioFocus(focusListener) } catch (_: Throwable) {}
        }
        focusRequest = null
    }

    @RequiresApi(Build.VERSION_CODES.S)
    private fun selectDevice(manager: AudioManager, preferEarpiece: Boolean): AudioDeviceInfo? {
        val devices = try { manager.availableCommunicationDevices } catch (_: Throwable) { return null }
        if (!preferEarpiece) {
            devices.firstOrNull { it.isSink && it.type == AudioDeviceInfo.TYPE_BUILTIN_SPEAKER }
                ?.let { return it }
        }
        devices.firstOrNull { device ->
            device.isSink && when (device.type) {
                AudioDeviceInfo.TYPE_BLUETOOTH_SCO,
                AudioDeviceInfo.TYPE_BLE_HEADSET,
                AudioDeviceInfo.TYPE_USB_HEADSET,
                AudioDeviceInfo.TYPE_WIRED_HEADSET,
                AudioDeviceInfo.TYPE_WIRED_HEADPHONES -> true
                else -> false
            }
        }?.let { return it }
        val builtIn = if (preferEarpiece) {
            AudioDeviceInfo.TYPE_BUILTIN_EARPIECE
        } else {
            AudioDeviceInfo.TYPE_BUILTIN_SPEAKER
        }
        return devices.firstOrNull { it.isSink && it.type == builtIn }
            ?: devices.firstOrNull { it.isSink && it.type == AudioDeviceInfo.TYPE_BUILTIN_SPEAKER }
    }

    private fun syncProximity(context: Context, preferEarpiece: Boolean) {
        if (!preferEarpiece) {
            releaseProximity(true)
            return
        }
        acquireProximity(context)
    }

    @SuppressLint("WakelockTimeout")
    private fun acquireProximity(context: Context) {
        val manager = context.getSystemService(PowerManager::class.java) ?: return
        if (!manager.isWakeLockLevelSupported(PowerManager.PROXIMITY_SCREEN_OFF_WAKE_LOCK)) return
        val wakeLock = proximityWakeLock ?: try {
            manager.newWakeLock(
                PowerManager.PROXIMITY_SCREEN_OFF_WAKE_LOCK,
                "Ratspeak:LXSTProximity",
            ).apply { setReferenceCounted(false) }.also { proximityWakeLock = it }
        } catch (_: Throwable) {
            null
        } ?: return
        if (!wakeLock.isHeld) try { wakeLock.acquire() } catch (_: Throwable) {}
    }

    private fun releaseProximity(waitForNoProximity: Boolean) {
        val wakeLock = proximityWakeLock ?: return
        try {
            if (wakeLock.isHeld) {
                if (waitForNoProximity) {
                    wakeLock.release(PowerManager.RELEASE_FLAG_WAIT_FOR_NO_PROXIMITY)
                } else {
                    wakeLock.release()
                }
            }
        } catch (_: Throwable) {}
        proximityWakeLock = null
    }
}
