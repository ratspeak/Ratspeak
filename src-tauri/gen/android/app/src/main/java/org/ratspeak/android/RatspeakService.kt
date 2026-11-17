package org.ratspeak.android

import android.app.*
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.wifi.WifiManager
import android.os.Build
import android.os.IBinder
import android.os.Looper
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

class RatspeakService : Service() {
    companion object {
        private const val TAG = "RatspeakService"
        const val CHANNEL_ID = "ratspeak_service"
        const val MSG_CHANNEL_ID = "ratspeak_messages"
        const val CALL_CHANNEL_ID = "ratspeak_calls"
        const val NOTIFICATION_ID = 1
        const val ACTION_STOP = "STOP"
        const val ACTION_ENABLE_MULTICAST = "ENABLE_MULTICAST"
        const val ACTION_DISABLE_MULTICAST = "DISABLE_MULTICAST"
        private const val SERVICE_READY_TIMEOUT_MS = 1_500L
        internal const val MICROPHONE_FAILURE_NONE = ""
        internal const val MICROPHONE_FAILURE_OWNER_BUSY = "owner_busy"
        internal const val MICROPHONE_FAILURE_SERVICE_START = "service_start"
        internal const val MICROPHONE_FAILURE_SERVICE_TIMEOUT = "service_timeout"
        internal const val MICROPHONE_FAILURE_FOREGROUND_TYPE = "foreground_type"
        private val serviceReadyLock = Any()
        @Volatile private var activeService: RatspeakService? = null
        @Volatile private var lastMicrophoneFailure = MICROPHONE_FAILURE_NONE
        private var serviceReadyLatch = CountDownLatch(1)

        private fun reportMicrophoneFailure(code: String) {
            lastMicrophoneFailure = code
            // Stable phase codes contain no identity, session token, route or device name.
            Log.w(TAG, "microphone_transition_failed code=$code")
        }

        internal fun lastMicrophoneFailureCode(): String = lastMicrophoneFailure

        /** No session left for this service to keep alive: a failed start, or Shut down. */
        internal fun stopActive() {
            val service = activeService ?: return
            service.stopForeground(Service.STOP_FOREGROUND_REMOVE)
            service.stopSelf()
        }

        private fun publishReady(service: RatspeakService) {
            synchronized(serviceReadyLock) {
                activeService = service
                serviceReadyLatch.countDown()
            }
        }

        private fun publishStopped(service: RatspeakService) {
            synchronized(serviceReadyLock) {
                if (activeService !== service) return
                activeService = null
                serviceReadyLatch = CountDownLatch(1)
            }
        }

        /**
         * Rust invokes microphone admission from a blocking worker. If Android
         * is recreating the process service, wait briefly for onCreate instead
         * of turning that normal lifecycle edge into a permanent recorder
         * failure. Main-looper callers start the service but never wait.
         */
        internal fun ensureReadyForMicrophoneCapture(context: Context): Boolean {
            activeService?.let { return true }
            if (!RatspeakNativeBridge.hasActivitySession()) {
                reportMicrophoneFailure(MICROPHONE_FAILURE_SERVICE_START)
                return false
            }
            val application = context.applicationContext
            val callerIsMainThread = Looper.myLooper() == Looper.getMainLooper()
            val plan = RatspeakMobilePolicy.serviceReadinessPlan(
                serviceReady = activeService != null,
                callerIsMainThread = callerIsMainThread,
            )
            if (plan == RatspeakMobilePolicy.ServiceReadinessPlan.READY) return true

            val ready = synchronized(serviceReadyLock) {
                activeService?.let { return true }
                serviceReadyLatch
            }
            try {
                ContextCompat.startForegroundService(
                    application,
                    Intent(application, RatspeakService::class.java),
                )
            } catch (_: Throwable) {
                reportMicrophoneFailure(MICROPHONE_FAILURE_SERVICE_START)
                return false
            }
            if (plan == RatspeakMobilePolicy.ServiceReadinessPlan.START_WITHOUT_WAIT) {
                reportMicrophoneFailure(MICROPHONE_FAILURE_SERVICE_TIMEOUT)
                return false
            }
            val signalled = try {
                ready.await(SERVICE_READY_TIMEOUT_MS, TimeUnit.MILLISECONDS)
            } catch (_: InterruptedException) {
                Thread.currentThread().interrupt()
                false
            }
            if (!signalled || activeService == null) {
                reportMicrophoneFailure(MICROPHONE_FAILURE_SERVICE_TIMEOUT)
                return false
            }
            lastMicrophoneFailure = MICROPHONE_FAILURE_NONE
            return true
        }

        /** Promote the running service before opening exact-session microphone capture. */
        internal fun setMicrophoneCaptureActive(
            context: Context,
            ownerToken: String,
            active: Boolean,
        ): Boolean {
            if (!active) {
                val service = activeService ?: return true
                return service.setMicrophoneCaptureActive(ownerToken, false)
            }
            if (!ensureReadyForMicrophoneCapture(context)) return false
            val service = activeService ?: return false
            return service.setMicrophoneCaptureActive(ownerToken, true)
        }
    }

    private var lastKnownPeerCount: Int = -1
    private var multicastLock: WifiManager.MulticastLock? = null
    private var sessionAdmitted = false
    @Volatile private var running = true
    @Volatile private var microphoneCaptureOwner: String? = null

    override fun onCreate() {
        super.onCreate()
        // Android can redeliver a pending start even with START_NOT_STICKY.
        // A service-only process has no Rust bootstrap or identity-unlock UI.
        // Never leave a misleading foreground notification or observers alive
        // after process death. An explicit Activity launch authorizes only this
        // process lifetime; removing its task does not revoke the live session.
        // A core that failed to start has no session at all.
        if (!RatspeakNativeBridge.hasActivitySession() || RatspeakNativeBridge.coreStartupFailed) {
            running = false
            // A pending foreground start still requires promotion, even when
            // immediately rejected. stopSelf() alone makes Android crash this
            // process for an unfulfilled startForegroundService obligation.
            // Satisfy only that handshake, then remove its truthful temporary
            // card. Never publish a session, start observers or unlock here.
            createNotificationChannel()
            startForegroundTyped(microphoneCapture = false)
            stopForeground(STOP_FOREGROUND_REMOVE)
            stopSelf()
            return
        }
        sessionAdmitted = true
        createNotificationChannel()
        createMessageNotificationChannel()
        createCallNotificationChannel()
        startForegroundTyped(microphoneCapture = false)
        publishReady(this)
        RatspeakPlatformSupervisor.start(this)
        // Message and call notifications are driven by the Rust/Tauri notification backend.
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (!sessionAdmitted) {
            stopSelf()
            return START_NOT_STICKY
        }
        when (intent?.action) {
            ACTION_STOP -> {
                running = false
                stopForeground(STOP_FOREGROUND_REMOVE)
                stopSelf()
                return START_NOT_STICKY
            }
            ACTION_ENABLE_MULTICAST -> acquireMulticastLock()
            ACTION_DISABLE_MULTICAST -> releaseMulticastLock()
        }
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        running = false
        RatspeakPlatformSupervisor.stop(this)
        releaseMulticastLock()
        publishStopped(this)
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    internal fun setMulticastNeeded(enable: Boolean) {
        if (enable) acquireMulticastLock() else releaseMulticastLock()
    }

    private fun acquireMulticastLock() {
        if (multicastLock?.isHeld == true) return
        try {
            val wifi = applicationContext.getSystemService(Context.WIFI_SERVICE) as? WifiManager
            multicastLock = wifi?.createMulticastLock("ratspeak-multicast")?.apply {
                setReferenceCounted(false)
                acquire()
            }
            Log.i(TAG, "MulticastLock acquired=${multicastLock?.isHeld == true}")
        } catch (e: Exception) {
            Log.w(TAG, "MulticastLock acquire failed: ${e.message}")
        }
    }

    private fun releaseMulticastLock() {
        try {
            multicastLock?.takeIf { it.isHeld }?.release()
        } catch (_: Exception) {
        }
        multicastLock = null
    }

    private fun createNotificationChannel() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        val channel = NotificationChannel(
            CHANNEL_ID, "Ratspeak Background",
            NotificationManager.IMPORTANCE_LOW
        ).apply { description = "Keeps mesh network connections active" }
        getSystemService(NotificationManager::class.java).createNotificationChannel(channel)
    }

    private fun createMessageNotificationChannel() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        val channel = NotificationChannel(
            MSG_CHANNEL_ID, "Messages",
            NotificationManager.IMPORTANCE_HIGH
        ).apply { description = "New message notifications" }
        getSystemService(NotificationManager::class.java).createNotificationChannel(channel)
    }

    private fun createCallNotificationChannel() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        val channel = NotificationChannel(
            CALL_CHANNEL_ID, "Calls",
            NotificationManager.IMPORTANCE_HIGH
        ).apply {
            description = "Incoming call notifications"
            enableVibration(true)
            lockscreenVisibility = Notification.VISIBILITY_PUBLIC
        }
        getSystemService(NotificationManager::class.java).createNotificationChannel(channel)
    }

    private fun buildNotification(peerCount: Int = -1): Notification {
        val openIntent = packageManager.getLaunchIntentForPackage(packageName)
        val pendingIntent = PendingIntent.getActivity(
            this, 0, openIntent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )
        val text = when {
            !sessionAdmitted -> "Open Ratspeak to resume"
            microphoneCaptureOwner != null -> "Microphone active"
            peerCount < 0 -> "Ratspeak is running"
            peerCount == 0 -> "Active · no peers connected"
            peerCount == 1 -> "Active · 1 peer connected"
            else -> "Active · $peerCount peers connected"
        }
        return NotificationCompat.Builder(this, CHANNEL_ID)
            .setContentTitle("Ratspeak")
            .setContentText(text)
            .setSmallIcon(R.drawable.ic_notification)
            .setPriority(NotificationCompat.PRIORITY_LOW)
            .setOngoing(true)
            .setContentIntent(pendingIntent)
            .build()
    }

    @Synchronized
    private fun setMicrophoneCaptureActive(ownerToken: String, active: Boolean): Boolean {
        if (!RatspeakMobilePolicy.validCallSessionToken(ownerToken)) {
            reportMicrophoneFailure(MICROPHONE_FAILURE_FOREGROUND_TYPE)
            return false
        }
        val previous = microphoneCaptureOwner
        when (RatspeakMobilePolicy.microphoneCapturePlan(previous, ownerToken, active)) {
            RatspeakMobilePolicy.MicrophoneCapturePlan.ALREADY_ACTIVE,
            RatspeakMobilePolicy.MicrophoneCapturePlan.ALREADY_INACTIVE -> {
                lastMicrophoneFailure = MICROPHONE_FAILURE_NONE
                return true
            }
            RatspeakMobilePolicy.MicrophoneCapturePlan.REJECT -> {
                reportMicrophoneFailure(MICROPHONE_FAILURE_OWNER_BUSY)
                return false
            }
            RatspeakMobilePolicy.MicrophoneCapturePlan.PROMOTE -> {
                microphoneCaptureOwner = ownerToken
            }
            RatspeakMobilePolicy.MicrophoneCapturePlan.DEMOTE -> {
                microphoneCaptureOwner = null
            }
        }
        return try {
            startForegroundTyped(microphoneCapture = active)
            lastMicrophoneFailure = MICROPHONE_FAILURE_NONE
            true
        } catch (_: Throwable) {
            microphoneCaptureOwner = previous
            // Preserve the last known-good foreground type and truthful card
            // if Android rejects a promotion (for example, permission denied).
            try { startForegroundTyped(microphoneCapture = previous != null) } catch (_: Throwable) {}
            reportMicrophoneFailure(MICROPHONE_FAILURE_FOREGROUND_TYPE)
            false
        }
    }

    private fun startForegroundTyped(microphoneCapture: Boolean) {
        val type = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            var value = ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE
            if (microphoneCapture && Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                value = value or ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
            }
            value
        } else {
            0
        }
        ServiceCompat.startForeground(
            this,
            NOTIFICATION_ID,
            buildNotification(),
            type,
        )
    }

    // Kept for a future foreground-card peer-count bridge.
    @Suppress("unused")
    private fun refreshForegroundNotification(peerCount: Int) {
        if (peerCount == lastKnownPeerCount) return
        lastKnownPeerCount = peerCount
        getSystemService(NotificationManager::class.java)
            .notify(NOTIFICATION_ID, buildNotification(peerCount))
    }
}
