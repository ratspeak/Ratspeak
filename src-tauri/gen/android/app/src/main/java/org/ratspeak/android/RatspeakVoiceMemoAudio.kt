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
    @Volatile private var lastStartFailure = ""
    private var focusListener: AudioManager.OnAudioFocusChangeListener? = null
    private var focusRequest: AudioFocusRequest? = null
    @Volatile private var ownerToken: String? = null
    @Volatile private var playbackOwnerToken: String? = null
    private var previousMode = AudioManager.MODE_NORMAL
    private var cleanupRetryToken: String? = null
    private var cleanupRetryAttempt = 0

    @JvmStatic
    fun isActive(): Boolean = ownerToken != null || playbackOwnerToken != null

    @JvmStatic
    fun isSessionActive(sessionToken: String): Boolean = ownerToken == sessionToken

    /** Stable, non-sensitive failure phase for Rust diagnostics and adb qualification. */
    @JvmStatic
    fun lastStartFailureCode(): String = lastStartFailure

    private fun fail(code: String, result: Int): Int {
        lastStartFailure = code
        // Never include the opaque session token or selected audio device.
        Log.w(TAG, "start_failed code=$code")
        return result
    }

    /**
     * Acquire focus, communication-mode microphone routing, and the microphone
     * foreground-service type before Oboe opens AudioRecord. The opaque token
     * comes from the Rust recording generation and fences every later cleanup.
     */
    @JvmStatic
    fun startForSession(context: Context, sessionToken: String): Int = synchronized(lock) {
        if (!RatspeakMobilePolicy.validCallSessionToken(sessionToken)) {
            return fail("invalid_session", START_PLATFORM_UNAVAILABLE)
        }
        if (ownerToken == sessionToken) return START_OK
        if (ownerToken != null || playbackOwnerToken != null || RatspeakCallAudio.isActive()) {
            return fail("audio_owner_busy", START_BUSY)
        }

        val application = context.applicationContext
        if (!RatspeakService.ensureReadyForMicrophoneCapture(application)) {
            return fail(
                "service_${RatspeakService.lastMicrophoneFailureCode().ifEmpty { "unavailable" }}",
                START_PLATFORM_UNAVAILABLE,
            )
        }
        val manager = application.getSystemService(AudioManager::class.java)
            ?: return fail("audio_manager", START_PLATFORM_UNAVAILABLE)
        previousMode = manager.mode
        if (!requestFocus(manager, application, sessionToken)) {
            return fail("audio_focus", START_BUSY)
        }
        try {
            // This is the same capture mode under which LXST calls reliably
            // open Oboe, without selecting a call-only earpiece/speaker route.
            manager.mode = AudioManager.MODE_IN_COMMUNICATION
        } catch (_: Throwable) {
            releaseAudio(manager)
            return fail("communication_mode", START_PLATFORM_UNAVAILABLE)
        }
        if (!RatspeakService.setMicrophoneCaptureActive(application, sessionToken, true)) {
            releaseAudio(manager)
            val serviceFailure = RatspeakService.lastMicrophoneFailureCode()
            return fail(
                "service_${serviceFailure.ifEmpty { "promotion" }}",
                if (serviceFailure == RatspeakService.MICROPHONE_FAILURE_OWNER_BUSY) {
                    START_BUSY
                } else {
                    START_PLATFORM_UNAVAILABLE
                },
            )
        }
        cleanupRetryToken = null
        cleanupRetryAttempt = 0
        ownerToken = sessionToken
        lastStartFailure = ""
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

    /** Acquire an exact media-output lease without promoting microphone capture. */
    @JvmStatic
    fun startPlaybackForSession(context: Context, sessionToken: String): Int = synchronized(lock) {
        if (!RatspeakMobilePolicy.validCallSessionToken(sessionToken)) {
            return fail("invalid_playback_session", START_PLATFORM_UNAVAILABLE)
        }
        if (playbackOwnerToken == sessionToken) return START_OK
        if (ownerToken != null || playbackOwnerToken != null || RatspeakCallAudio.isActive()) {
            return fail("audio_owner_busy", START_BUSY)
        }

        val application = context.applicationContext
        val manager = application.getSystemService(AudioManager::class.java)
            ?: return fail("audio_manager", START_PLATFORM_UNAVAILABLE)
        previousMode = manager.mode
        if (!requestPlaybackFocus(manager, application, sessionToken)) {
            return fail("playback_audio_focus", START_BUSY)
        }
        try {
            manager.mode = AudioManager.MODE_NORMAL
        } catch (_: Throwable) {
            releaseAudio(manager)
            return fail("playback_mode", START_PLATFORM_UNAVAILABLE)
        }
        playbackOwnerToken = sessionToken
        lastStartFailure = ""
        START_OK
    }

    @JvmStatic
    fun stopPlaybackForSession(context: Context, sessionToken: String): Boolean = synchronized(lock) {
        if (!RatspeakMobilePolicy.validCallSessionToken(sessionToken) ||
            playbackOwnerToken != sessionToken
        ) {
            return false
        }
        finishPlaybackStop(context.applicationContext, sessionToken)
        true
    }

    private fun requestFocus(manager: AudioManager, context: Context, sessionToken: String): Boolean {
        lateinit var listener: AudioManager.OnAudioFocusChangeListener
        listener = AudioManager.OnAudioFocusChangeListener { change ->
            if (change != AudioManager.AUDIOFOCUS_LOSS &&
                change != AudioManager.AUDIOFOCUS_LOSS_TRANSIENT
            ) return@OnAudioFocusChangeListener
            handler.post { handleFocusLoss(context, sessionToken, listener) }
        }
        val result = try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                val attributes = AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_VOICE_COMMUNICATION)
                    .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
                    .build()
                val request = AudioFocusRequest.Builder(
                    AudioManager.AUDIOFOCUS_GAIN_TRANSIENT_EXCLUSIVE,
                )
                    .setAudioAttributes(attributes)
                    .setOnAudioFocusChangeListener(listener, handler)
                    .build()
                val focusResult = manager.requestAudioFocus(request)
                if (focusResult == AudioManager.AUDIOFOCUS_REQUEST_GRANTED) {
                    focusListener = listener
                    focusRequest = request
                }
                focusResult
            } else {
                @Suppress("DEPRECATION")
                val focusResult = manager.requestAudioFocus(
                    listener,
                    AudioManager.STREAM_VOICE_CALL,
                    AudioManager.AUDIOFOCUS_GAIN_TRANSIENT_EXCLUSIVE,
                )
                if (focusResult == AudioManager.AUDIOFOCUS_REQUEST_GRANTED) {
                    focusListener = listener
                }
                focusResult
            }
        } catch (_: Throwable) {
            AudioManager.AUDIOFOCUS_REQUEST_FAILED
        }
        return result == AudioManager.AUDIOFOCUS_REQUEST_GRANTED
    }

    private fun requestPlaybackFocus(
        manager: AudioManager,
        context: Context,
        sessionToken: String,
    ): Boolean {
        lateinit var listener: AudioManager.OnAudioFocusChangeListener
        listener = AudioManager.OnAudioFocusChangeListener { change ->
            if (change != AudioManager.AUDIOFOCUS_LOSS &&
                change != AudioManager.AUDIOFOCUS_LOSS_TRANSIENT
            ) return@OnAudioFocusChangeListener
            handler.post { handlePlaybackFocusLoss(context, sessionToken, listener) }
        }
        val result = try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                val attributes = AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_MEDIA)
                    .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
                    .build()
                val request = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT)
                    .setAudioAttributes(attributes)
                    .setOnAudioFocusChangeListener(listener, handler)
                    .build()
                val focusResult = manager.requestAudioFocus(request)
                if (focusResult == AudioManager.AUDIOFOCUS_REQUEST_GRANTED) {
                    focusListener = listener
                    focusRequest = request
                }
                focusResult
            } else {
                @Suppress("DEPRECATION")
                val focusResult = manager.requestAudioFocus(
                    listener,
                    AudioManager.STREAM_MUSIC,
                    AudioManager.AUDIOFOCUS_GAIN_TRANSIENT,
                )
                if (focusResult == AudioManager.AUDIOFOCUS_REQUEST_GRANTED) {
                    focusListener = listener
                }
                focusResult
            }
        } catch (_: Throwable) {
            AudioManager.AUDIOFOCUS_REQUEST_FAILED
        }
        return result == AudioManager.AUDIOFOCUS_REQUEST_GRANTED
    }

    private fun handleFocusLoss(
        context: Context,
        sessionToken: String,
        listener: AudioManager.OnAudioFocusChangeListener,
    ) {
        val current = synchronized(lock) {
            RatspeakMobilePolicy.voiceMemoInterruptionOwns(
                ownerToken,
                sessionToken,
                focusListener === listener,
            )
        }
        if (!current) return
        Log.w(TAG, "audio_interrupted code=focus_loss")
        if (!RatspeakAndroidObservers.voiceMemoAudioInterruption(sessionToken)) {
            // With no visible Activity there is no UI owner to request Rust
            // cancellation. Release the exact native lease so focus and the
            // microphone foreground type cannot leak in the background.
            stopForSession(context, sessionToken)
        }
    }

    private fun handlePlaybackFocusLoss(
        context: Context,
        sessionToken: String,
        listener: AudioManager.OnAudioFocusChangeListener,
    ) = synchronized(lock) {
        if (playbackOwnerToken != sessionToken || focusListener !== listener) return@synchronized
        Log.w(TAG, "playback_interrupted code=focus_loss")
        RatspeakVoiceAudio.stop()
        finishPlaybackStop(context.applicationContext, sessionToken)
    }

    private fun finishStop(context: Context, sessionToken: String) {
        if (ownerToken != sessionToken) return
        ownerToken = null
        cleanupRetryToken = null
        cleanupRetryAttempt = 0
        context.getSystemService(AudioManager::class.java)?.let(::releaseAudio)
    }

    private fun finishPlaybackStop(context: Context, sessionToken: String) {
        if (playbackOwnerToken != sessionToken) return
        playbackOwnerToken = null
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
            focusListener?.let { listener ->
                try { manager.abandonAudioFocus(listener) } catch (_: Throwable) {}
            }
        }
        focusListener = null
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
