package org.ratspeak.android

import java.io.PushbackInputStream
import java.net.ServerSocket
import java.net.Socket
import java.net.SocketTimeoutException

/** Filters closed clients left in the stable BLE listener backlog. */
object RatspeakBleSocketQueue {
    private const val ACCEPT_POLL_MS = 1_000
    private const val STALE_CLIENT_PROBE_MS = 250

    data class AcceptedClient(
        val socket: Socket,
        val input: PushbackInputStream,
    )

    fun acceptUsable(
        listener: ServerSocket,
        shouldContinue: () -> Boolean,
        staleProbeMs: Int = STALE_CLIENT_PROBE_MS,
    ): AcceptedClient? {
        listener.soTimeout = ACCEPT_POLL_MS
        while (shouldContinue()) {
            val socket = try {
                listener.accept()
            } catch (_: SocketTimeoutException) {
                continue
            }
            val input = try {
                socket.soTimeout = staleProbeMs.coerceAtLeast(1)
                PushbackInputStream(socket.getInputStream(), 1)
            } catch (_: Throwable) {
                try { socket.close() } catch (_: Throwable) {}
                continue
            }
            try {
                val first = input.read()
                if (first < 0) {
                    socket.close()
                    continue
                }
                input.unread(first)
            } catch (_: SocketTimeoutException) {
                // An open Rust client is allowed to be idle before init.
            } catch (_: Throwable) {
                try { socket.close() } catch (_: Throwable) {}
                continue
            }
            socket.soTimeout = 0
            return AcceptedClient(socket, input)
        }
        return null
    }
}
