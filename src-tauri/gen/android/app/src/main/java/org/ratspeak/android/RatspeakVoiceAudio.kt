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
    private var trackStarted = false
    private var pcm16Scratch = ShortArray(0)
    private var lastError = ""

    @JvmStatic
    fun start(sampleRate: Int, channels: Int): Boolean {
        val safeSampleRate = sampleRate.coerceIn(8_000, 48_000)
        val safeChannels = channels.coerceIn(1, 2)
        synchronized(lock) {
            val existing = track
            if (
                existing != null &&
                existing.state == AudioTrack.STATE_INITIALIZED &&
                trackSampleRate == safeSampleRate &&
                trackChannels == safeChannels
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
            for (encoding in intArrayOf(AudioFormat.ENCODING_PCM_FLOAT, AudioFormat.ENCODING_PCM_16BIT)) {
                val created = createTrack(safeSampleRate, safeChannels, channelMask, encoding, errors)
                    ?: continue
                try {
                    created.setVolume(AudioTrack.getMaxVolume())
                    configureStartThreshold(created, safeSampleRate)
                    track = created
                    trackSampleRate = safeSampleRate
                    trackChannels = safeChannels
                    trackEncoding = encoding
                    trackStarted = false
                    lastError = ""
                    return true
                } catch (e: Throwable) {
                    errors.add("${encodingName(encoding)} prepare failed: ${e.message ?: e.javaClass.simpleName}")
                    try { created.release() } catch (_: Throwable) {}
                    track = null
                    trackSampleRate = 0
                    trackChannels = 0
                    trackEncoding = AudioFormat.ENCODING_INVALID
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
                val writeMode = if (starting) AudioTrack.WRITE_BLOCKING else AudioTrack.WRITE_NON_BLOCKING
                val written = if (trackEncoding == AudioFormat.ENCODING_PCM_16BIT) {
                    writePcm16(active, samples, count, writeMode)
                } else {
                    active.write(samples, 0, count, writeMode)
                }
                if (written > 0 && starting) {
                    active.play()
                    trackStarted = true
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

    private fun createTrack(
        sampleRate: Int,
        channels: Int,
        channelMask: Int,
        encoding: Int,
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
            .setUsage(AudioAttributes.USAGE_VOICE_COMMUNICATION)
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

    private fun configureStartThreshold(active: AudioTrack, sampleRate: Int) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S) return
        try {
            val desiredFrames = (sampleRate * START_THRESHOLD_MS / 1000).coerceAtLeast(1)
            val capacityFrames = active.bufferCapacityInFrames.coerceAtLeast(1)
            active.setStartThresholdInFrames(desiredFrames.coerceAtMost(capacityFrames))
        } catch (_: Throwable) {
            // Optional latency tuning. Older or unusual sinks keep their
            // platform-selected threshold while retaining write-before-play.
        }
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
        track = null
        trackSampleRate = 0
        trackChannels = 0
        trackEncoding = AudioFormat.ENCODING_INVALID
        trackStarted = false
        try { current.pause() } catch (_: Throwable) {}
        try { current.flush() } catch (_: Throwable) {}
        try { current.stop() } catch (_: Throwable) {}
        try { current.release() } catch (_: Throwable) {}
    }

    private fun encodingName(encoding: Int): String {
        return if (encoding == AudioFormat.ENCODING_PCM_16BIT) "pcm16" else "float"
    }
}
