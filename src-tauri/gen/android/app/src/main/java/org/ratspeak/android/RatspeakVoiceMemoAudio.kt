package org.ratspeak.android

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.os.Build
import android.os.Handler
import android.os.Looper

/** Exact-session Android audio ownership for Rust voice-memo capture. */
object RatspeakVoiceMemoAudio {
    private const val TAG = "RatspeakVoiceMemoAudio"
    private const val START_OK = 0
    private const val START_BUSY = 1
    private const val START_PLATFORM_UNAVAILABLE = 2
    private const val MAX_CLEANUP_RETRIES = 5
    private val lock = Any()
    private val handler = Handler(Looper.getMainLooper())
    private val focusListener = AudioManager.OnAudioFocusChangeListener { }
    private var focusRequest: AudioFocusRequest? = null
    @Volatile private var ownerToken: String? = null
    private var previousMode = AudioManager.MODE_NORMAL
    private var cleanupRetryToken: String? = null
    private var cleanupRetryAttempt = 0

    @JvmStatic
    fun isActive(): Boolean = ownerToken != null

    /**
     * Acquire focus, communication-mode microphone routing, and the microphone
     * foreground-service type before Oboe opens AudioRecord. The opaque token
     * comes from the Rust recording generation and fences every later cleanup.
     */
    @JvmStatic
    fun startForSession(context: Context, sessionToken: String): Int = synchronized(lock) {
        if (!RatspeakMobilePolicy.validCallSessionToken(sessionToken)) {
            return START_PLATFORM_UNAVAILABLE
        }
        if (ownerToken == sessionToken) return START_OK
        if (ownerToken != null || RatspeakCallAudio.isActive()) return START_BUSY

        val application = context.applicationContext
        val manager = application.getSystemService(AudioManager::class.java)
            ?: return START_PLATFORM_UNAVAILABLE
        previousMode = manager.mode
        if (!requestFocus(manager)) return START_BUSY
        try {
            // This is the same capture mode under which LXST calls reliably
            // open Oboe, without selecting a call-only earpiece/speaker route.
            manager.mode = AudioManager.MODE_IN_COMMUNICATION
        } catch (_: Throwable) {
            releaseAudio(manager)
            return START_PLATFORM_UNAVAILABLE
        }
        if (!RatspeakService.setMicrophoneCaptureActive(application, sessionToken, true)) {
            releaseAudio(manager)
            return START_PLATFORM_UNAVAILABLE
        }
        cleanupRetryToken = null
        cleanupRetryAttempt = 0
        ownerToken = sessionToken
        START_OK
    }

    /** Stale Rust task cleanup cannot release a replacement recording. */
    @JvmStatic
    fun stopForSession(context: Context, sessionToken: String): Boolean = synchronized(lock) {
        if (!RatspeakMobilePolicy.validCallSessionToken(sessionToken) || ownerToken != sessionToken) {
            return false
        }
        val application = context.applicationContext
        if (!RatspeakService.setMicrophoneCaptureActive(application, sessionToken, false)) {
            scheduleCleanup(application, sessionToken)
            return false
        }
        finishStop(application, sessionToken)
        true
    }

    private fun requestFocus(manager: AudioManager): Boolean {
        val result = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val attributes = AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_VOICE_COMMUNICATION)
                .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
                .build()
            val request = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT_EXCLUSIVE)
                .setAudioAttributes(attributes)
                .setOnAudioFocusChangeListener(focusListener, handler)
                .build()
            val focusResult = manager.requestAudioFocus(request)
            if (focusResult == AudioManager.AUDIOFOCUS_REQUEST_GRANTED) focusRequest = request
            focusResult
        } else {
            @Suppress("DEPRECATION")
            manager.requestAudioFocus(
                focusListener,
                AudioManager.STREAM_VOICE_CALL,
                AudioManager.AUDIOFOCUS_GAIN_TRANSIENT_EXCLUSIVE,
            )
        }
        return result == AudioManager.AUDIOFOCUS_REQUEST_GRANTED
    }

    private fun finishStop(context: Context, sessionToken: String) {
        if (ownerToken != sessionToken) return
        ownerToken = null
        cleanupRetryToken = null
        cleanupRetryAttempt = 0
        context.getSystemService(AudioManager::class.java)?.let(::releaseAudio)
    }

    private fun releaseAudio(manager: AudioManager) {
        if (!RatspeakCallAudio.isActive()) {
            try { manager.mode = previousMode } catch (_: Throwable) {}
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            focusRequest?.let {
                try { manager.abandonAudioFocusRequest(it) } catch (_: Throwable) {}
            }
        } else {
            @Suppress("DEPRECATION")
            try { manager.abandonAudioFocus(focusListener) } catch (_: Throwable) {}
        }
        focusRequest = null
    }

    private fun scheduleCleanup(context: Context, sessionToken: String) {
        if (cleanupRetryToken == sessionToken) return
        cleanupRetryToken = sessionToken
        cleanupRetryAttempt = 0
        handler.postDelayed(object : Runnable {
            override fun run() {
                synchronized(lock) {
                    if (ownerToken != sessionToken) {
                        if (cleanupRetryToken == sessionToken) cleanupRetryToken = null
                        return
                    }
                    if (RatspeakService.setMicrophoneCaptureActive(context, sessionToken, false)) {
                        finishStop(context, sessionToken)
                    } else {
                        cleanupRetryAttempt += 1
                        if (cleanupRetryAttempt < MAX_CLEANUP_RETRIES) {
                            handler.postDelayed(this, 30_000L)
                        } else {
                            cleanupRetryToken = null
                            Log.w(TAG, "Microphone foreground cleanup exhausted bounded retries")
                        }
                    }
                }
            }
        }, 2_000L)
    }
}
