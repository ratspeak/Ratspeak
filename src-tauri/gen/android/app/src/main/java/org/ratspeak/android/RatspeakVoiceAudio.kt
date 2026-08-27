package org.ratspeak.android

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import android.os.Build
import kotlin.math.max
import kotlin.math.min
import kotlin.math.roundToInt

object RatspeakVoiceAudio {
    private const val BYTES_PER_FLOAT_SAMPLE = 4
    private const val BYTES_PER_PCM16_SAMPLE = 2
    private const val TARGET_BUFFER_MS = 220
    private const val START_THRESHOLD_MS = 40
    private val lock = Any()

    private var track: AudioTrack? = null
    private var trackSampleRate = 0
    private var trackChannels = 0
    private var trackEncoding = AudioFormat.ENCODING_INVALID
    private var trackUsage = AudioAttributes.USAGE_UNKNOWN
    private var trackStarted = false
    private var trackSubmittedFrames = 0L
    private var voiceMemoStartupFrames = 0
    private var lastPlaybackHeadFrames = 0L
    private var pcm16Scratch = ShortArray(0)
    private var floatSilenceScratch = FloatArray(0)
    private var pcm16SilenceScratch = ShortArray(0)
    private var lastError = ""

    @JvmStatic
    fun isActive(): Boolean = synchronized(lock) {
        track?.state == AudioTrack.STATE_INITIALIZED
    }

    @JvmStatic
    fun start(sampleRate: Int, channels: Int): Boolean {
        return startWithUsage(sampleRate, channels, AudioAttributes.USAGE_VOICE_COMMUNICATION)
    }

    /** Finite voice-message playback uses the media route, not the call route. */
    @JvmStatic
    fun startVoiceMemoPlayback(sampleRate: Int, channels: Int): Boolean {
        return startWithUsage(sampleRate, channels, AudioAttributes.USAGE_MEDIA)
    }

    private fun startWithUsage(sampleRate: Int, channels: Int, usage: Int): Boolean {
        val safeSampleRate = sampleRate.coerceIn(8_000, 48_000)
        val safeChannels = channels.coerceIn(1, 2)
        synchronized(lock) {
            val existing = track
            if (
                existing != null &&
                existing.state == AudioTrack.STATE_INITIALIZED &&
                trackSampleRate == safeSampleRate &&
                trackChannels == safeChannels &&
                trackUsage == usage
            ) {
                return try {
                    if (trackStarted && existing.playState != AudioTrack.PLAYSTATE_PLAYING) {
                        existing.play()
                    }
                    lastError = ""
                    true
                } catch (e: Throwable) {
                    lastError = "existing AudioTrack play failed: ${e.message ?: e.javaClass.simpleName}"
                    stopLocked()
                    false
                }
            }

            stopLocked()
            val channelMask = if (safeChannels == 1) {
                AudioFormat.CHANNEL_OUT_MONO
            } else {
                AudioFormat.CHANNEL_OUT_STEREO
            }
            val errors = ArrayList<String>(2)
            val encodings = if (usage == AudioAttributes.USAGE_MEDIA) {
                // Finite memos favor the universally supported integer path.
                // Calls retain float-first output and its existing latency path.
                intArrayOf(AudioFormat.ENCODING_PCM_16BIT, AudioFormat.ENCODING_PCM_FLOAT)
            } else {
                intArrayOf(AudioFormat.ENCODING_PCM_FLOAT, AudioFormat.ENCODING_PCM_16BIT)
            }
            for (encoding in encodings) {
                val created = createTrack(
                    safeSampleRate,
                    safeChannels,
                    channelMask,
                    encoding,
                    usage,
                    errors,
                )
                    ?: continue
                try {
                    created.setVolume(AudioTrack.getMaxVolume())
                    val startupFrames = configureStartThreshold(created, safeSampleRate)
                    track = created
                    trackSampleRate = safeSampleRate
                    trackChannels = safeChannels
                    trackEncoding = encoding
                    trackUsage = usage
                    trackStarted = false
                    trackSubmittedFrames = 0L
                    voiceMemoStartupFrames = if (usage == AudioAttributes.USAGE_MEDIA) {
                        startupFrames
                    } else {
                        0
                    }
                    lastPlaybackHeadFrames = 0L
                    lastError = ""
                    return true
                } catch (e: Throwable) {
                    errors.add("${encodingName(encoding)} prepare failed: ${e.message ?: e.javaClass.simpleName}")
                    try { created.release() } catch (_: Throwable) {}
                    track = null
                    trackSampleRate = 0
                    trackChannels = 0
                    trackEncoding = AudioFormat.ENCODING_INVALID
                    trackUsage = AudioAttributes.USAGE_UNKNOWN
                    trackStarted = false
                }
            }
            lastError = errors.joinToString("; ").ifBlank { "Android voice AudioTrack could not be initialized" }
            return false
        }
    }

    @JvmStatic
    fun write(samples: FloatArray, length: Int): Int {
        synchronized(lock) {
            val active = track ?: return -1
            val count = min(length.coerceAtLeast(0), samples.size)
            if (count == 0) return 0
            return try {
                val starting = !trackStarted
                val memoPriming = trackUsage == AudioAttributes.USAGE_MEDIA && starting
                val memoAwaitingClock = trackUsage == AudioAttributes.USAGE_MEDIA &&
                    trackStarted &&
                    (active.playbackHeadPosition.toLong() and 0xffff_ffffL) == 0L
                val writeMode = if ((starting && !memoPriming) || memoAwaitingClock) {
                    AudioTrack.WRITE_BLOCKING
                } else {
                    AudioTrack.WRITE_NON_BLOCKING
                }
                val written = if (trackEncoding == AudioFormat.ENCODING_PCM_16BIT) {
                    writePcm16(active, samples, count, writeMode)
                } else {
                    active.write(samples, 0, count, writeMode)
                }
                if (written > 0) {
                    trackSubmittedFrames += written.toLong() / trackChannels.coerceAtLeast(1)
                    maybeStartAfterWrite(active)
                }
                written
            } catch (e: Throwable) {
                lastError = "AudioTrack write failed: ${e.message ?: e.javaClass.simpleName}"
                stopLocked()
                -1
            }
        }
    }

    @JvmStatic
    fun stop() {
        synchronized(lock) { stopLocked() }
    }

    @JvmStatic
    fun lastError(): String {
        synchronized(lock) {
            return lastError
        }
    }

    /** Unsigned AudioTrack playback-head frames, bounded well below wrap for a memo. */
    @JvmStatic
    fun playbackHeadFrames(): Long = synchronized(lock) {
        val active = track ?: return@synchronized lastPlaybackHeadFrames
        try {
            active.playbackHeadPosition.toLong() and 0xffff_ffffL
        } catch (_: Throwable) {
            lastPlaybackHeadFrames
        }
    }

    /** Exact native startup requirement used by the finite Rust memo feeder. */
    @JvmStatic
    fun voiceMemoStartupPrimeFrames(): Long = synchronized(lock) {
        if (trackUsage != AudioAttributes.USAGE_MEDIA) return@synchronized 0L
        voiceMemoStartupFrames.coerceAtLeast(1).toLong()
    }

    @JvmStatic
    fun voiceMemoPlaybackStarted(): Boolean = synchronized(lock) {
        trackUsage == AudioAttributes.USAGE_MEDIA &&
            trackStarted &&
            track?.playState == AudioTrack.PLAYSTATE_PLAYING
    }

    /**
     * A memo shorter than an older AudioTrack's startup threshold still needs
     * enough queued frames to start its playback clock. The added silence is a
     * native priming detail and is deliberately not counted in the Rust memo
     * duration or progress timeline.
     */
    @JvmStatic
    fun finishVoiceMemoInput(): Boolean = synchronized(lock) {
        val active = track ?: return@synchronized false
        if (
            active.state != AudioTrack.STATE_INITIALIZED ||
            trackUsage != AudioAttributes.USAGE_MEDIA ||
            trackSubmittedFrames <= 0L
        ) {
            return@synchronized false
        }
        var remainingFrames = RatspeakMobilePolicy.voiceMemoStartupPaddingFrames(
            voiceMemoStartupFrames,
            trackSubmittedFrames,
        )
        try {
            while (remainingFrames > 0L) {
                val chunkFrames = min(remainingFrames, 4_096L).toInt()
                val writtenSamples = writeSilenceFrames(active, chunkFrames)
                if (writtenSamples <= 0) {
                    lastError = "AudioTrack stopped accepting voice message startup padding"
                    return@synchronized false
                }
                val writtenFrames = writtenSamples / trackChannels.coerceAtLeast(1)
                if (writtenFrames <= 0) {
                    lastError = "AudioTrack accepted an incomplete voice message startup frame"
                    return@synchronized false
                }
                trackSubmittedFrames += writtenFrames.toLong()
                remainingFrames = (remainingFrames - writtenFrames).coerceAtLeast(0L)
                maybeStartAfterWrite(active)
            }
            maybeStartAfterWrite(active)
            if (!trackStarted) {
                lastError = "AudioTrack voice message did not reach its startup threshold"
                return@synchronized false
            }
            lastError = ""
            true
        } catch (e: Throwable) {
            lastError = "AudioTrack voice message startup padding failed: ${e.message ?: e.javaClass.simpleName}"
            false
        }
    }

    private fun createTrack(
        sampleRate: Int,
        channels: Int,
        channelMask: Int,
        encoding: Int,
        usage: Int,
        errors: MutableList<String>
    ): AudioTrack? {
        val minBuffer = try {
            AudioTrack.getMinBufferSize(sampleRate, channelMask, encoding)
        } catch (e: Throwable) {
            errors.add("${encodingName(encoding)} min buffer failed: ${e.message ?: e.javaClass.simpleName}")
            return null
        }
        if (minBuffer <= 0) {
            errors.add("${encodingName(encoding)} min buffer unavailable: $minBuffer")
            return null
        }
        val bytesPerSample = if (encoding == AudioFormat.ENCODING_PCM_16BIT) {
            BYTES_PER_PCM16_SAMPLE
        } else {
            BYTES_PER_FLOAT_SAMPLE
        }
        val frameBytes = (channels * bytesPerSample).coerceAtLeast(1)
        var targetBufferBytes = max(
            minBuffer * 2,
            sampleRate * channels * bytesPerSample * TARGET_BUFFER_MS / 1000
        )
        targetBufferBytes -= targetBufferBytes % frameBytes
        val attrs = AudioAttributes.Builder()
            .setUsage(usage)
            .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
            .build()
        val format = AudioFormat.Builder()
            .setEncoding(encoding)
            .setSampleRate(sampleRate)
            .setChannelMask(channelMask)
            .build()

        val created = try {
            AudioTrack.Builder()
                .setAudioAttributes(attrs)
                .setAudioFormat(format)
                .setBufferSizeInBytes(targetBufferBytes.coerceAtLeast(frameBytes))
                .setTransferMode(AudioTrack.MODE_STREAM)
                .build()
        } catch (e: Throwable) {
            errors.add("${encodingName(encoding)} build failed: ${e.message ?: e.javaClass.simpleName}")
            return null
        }
        if (created.state != AudioTrack.STATE_INITIALIZED) {
            errors.add("${encodingName(encoding)} state=${created.state}")
            try { created.release() } catch (_: Throwable) {}
            return null
        }
        return created
    }

    private fun configureStartThreshold(active: AudioTrack, sampleRate: Int): Int {
        val capacityFrames = active.bufferCapacityInFrames.coerceAtLeast(1)
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S) {
            // Before API 31 there is no public start-threshold control. Android
            // documents full-capacity priming as the portable streaming path.
            return RatspeakMobilePolicy.voiceMemoStartupFrames(
                Build.VERSION.SDK_INT,
                capacityFrames,
                capacityFrames,
            )
        }
        return try {
            val desiredFrames = (sampleRate * START_THRESHOLD_MS / 1000).coerceAtLeast(1)
            active.setStartThresholdInFrames(desiredFrames.coerceAtMost(capacityFrames))
            RatspeakMobilePolicy.voiceMemoStartupFrames(
                Build.VERSION.SDK_INT,
                capacityFrames,
                active.startThresholdInFrames,
            )
        } catch (_: Throwable) {
            // If an unusual sink rejects the low-latency threshold, prime the
            // full capacity rather than assuming the request was accepted.
            capacityFrames
        }
    }

    private fun writeSilenceFrames(active: AudioTrack, frames: Int): Int {
        val sampleCount = frames.coerceAtLeast(0) * trackChannels.coerceAtLeast(1)
        if (sampleCount == 0) return 0
        return if (trackEncoding == AudioFormat.ENCODING_PCM_16BIT) {
            if (pcm16SilenceScratch.size < sampleCount) {
                pcm16SilenceScratch = ShortArray(sampleCount)
            }
            active.write(pcm16SilenceScratch, 0, sampleCount, AudioTrack.WRITE_NON_BLOCKING)
        } else {
            if (floatSilenceScratch.size < sampleCount) {
                floatSilenceScratch = FloatArray(sampleCount)
            }
            active.write(floatSilenceScratch, 0, sampleCount, AudioTrack.WRITE_NON_BLOCKING)
        }
    }

    private fun maybeStartAfterWrite(active: AudioTrack) {
        if (trackStarted || trackSubmittedFrames <= 0L) return
        if (
            trackUsage == AudioAttributes.USAGE_MEDIA &&
            trackSubmittedFrames < voiceMemoStartupFrames.coerceAtLeast(1).toLong()
        ) {
            return
        }
        active.play()
        trackStarted = true
    }

    private fun writePcm16(
        active: AudioTrack,
        samples: FloatArray,
        count: Int,
        writeMode: Int
    ): Int {
        if (pcm16Scratch.size < count) {
            pcm16Scratch = ShortArray(count)
        }
        for (i in 0 until count) {
            val clamped = samples[i].coerceIn(-1.0f, 1.0f)
            pcm16Scratch[i] = (clamped * Short.MAX_VALUE.toFloat()).roundToInt().toShort()
        }
        return active.write(pcm16Scratch, 0, count, writeMode)
    }

    private fun stopLocked() {
        val current = track ?: return
        lastPlaybackHeadFrames = try {
            current.playbackHeadPosition.toLong() and 0xffff_ffffL
        } catch (_: Throwable) {
            lastPlaybackHeadFrames
        }
        track = null
        trackSampleRate = 0
        trackChannels = 0
        trackEncoding = AudioFormat.ENCODING_INVALID
        trackUsage = AudioAttributes.USAGE_UNKNOWN
        trackStarted = false
        trackSubmittedFrames = 0L
        voiceMemoStartupFrames = 0
        try { current.pause() } catch (_: Throwable) {}
        try { current.flush() } catch (_: Throwable) {}
        try { current.stop() } catch (_: Throwable) {}
        try { current.release() } catch (_: Throwable) {}
    }

    private fun encodingName(encoding: Int): String {
        return if (encoding == AudioFormat.ENCODING_PCM_16BIT) "pcm16" else "float"
    }
}
