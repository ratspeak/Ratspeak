package org.ratspeak.android

import android.os.Build
import android.os.SystemClock
import java.util.Locale

/**
 * Bounded, copyable record of the Bluetooth RNode connection, for testers on
 * phones where release builds log nothing. Phases, statuses and sizes only:
 * never addresses, device names or payload bytes.
 */
object RatspeakBleTrace {
    private const val MAX_LINES = 400
    private val lock = Any()
    private val lines = ArrayDeque<String>()
    private var startedAt = SystemClock.elapsedRealtime()

    fun begin() {
        synchronized(lock) {
            lines.clear()
            startedAt = SystemClock.elapsedRealtime()
        }
        add("Ratspeak ${BuildConfig.VERSION_NAME} on Android ${Build.VERSION.RELEASE} (API ${Build.VERSION.SDK_INT}), " +
            "${Build.MANUFACTURER} ${Build.MODEL}")
    }

    fun add(event: String) {
        synchronized(lock) {
            val elapsed = SystemClock.elapsedRealtime() - startedAt
            if (lines.size == MAX_LINES) lines.removeFirst()
            lines.addLast(String.format(Locale.ROOT, "+%d.%03ds %s", elapsed / 1000, elapsed % 1000, event))
        }
    }

    fun text(): String = synchronized(lock) { lines.joinToString("\n") }
}
