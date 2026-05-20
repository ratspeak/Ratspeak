package org.ratspeak.android

import android.app.*
import android.content.Context
import android.content.Intent
import android.net.wifi.WifiManager
import android.os.IBinder
import androidx.core.app.NotificationCompat
import org.json.JSONObject

class RatspeakService : Service() {
    companion object {
        private const val TAG = "RatspeakService"
        const val CHANNEL_ID = "ratspeak_service"
        const val MSG_CHANNEL_ID = "ratspeak_messages"
        const val CALL_CHANNEL_ID = "ratspeak_calls"
        const val NOTIFICATION_ID = 1
        // Reserved below 100 for system use (foreground / group summary).
        const val GROUP_SUMMARY_ID = 2
        const val MSG_NOTIFICATION_GROUP = "org.ratspeak.android.MESSAGES"
        // Per-sender notification IDs start at this offset; stable value per
        // dest_hash keeps the Android system tray updates on the same
        // notification instead of spawning a new card per poll.
        const val PER_SENDER_ID_OFFSET = 1000
        const val ACTION_STOP = "STOP"
        const val ACTION_REFRESH = "REFRESH"
        const val ACTION_ENABLE_MULTICAST = "ENABLE_MULTICAST"
        const val ACTION_DISABLE_MULTICAST = "DISABLE_MULTICAST"
    }

    private var lastKnownPeerCount: Int = -1
    // Per-sender state: key = dest_hash, value = (notificationId, lastUnreadCount)
    private val senderState = HashMap<String, Pair<Int, Int>>()
    private var multicastLock: WifiManager.MulticastLock? = null
    @Volatile private var running = true

    override fun onCreate() {
        super.onCreate()
        createNotificationChannel()
        createMessageNotificationChannel()
        createCallNotificationChannel()
        startForeground(NOTIFICATION_ID, buildNotification())
        // Message and call notifications are driven by the Rust/Tauri notification backend.
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> {
                running = false
                stopForeground(STOP_FOREGROUND_REMOVE)
                stopSelf()
                return START_NOT_STICKY
            }
            ACTION_REFRESH -> {
                // On foreground return, clear any stale per-sender
                // notifications — the WebView redraws unread badges from the
                // SQLite source of truth.
                clearAllMessageNotifications()
            }
            ACTION_ENABLE_MULTICAST -> acquireMulticastLock()
            ACTION_DISABLE_MULTICAST -> releaseMulticastLock()
        }
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        running = false
        releaseMulticastLock()
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

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
        val channel = NotificationChannel(
            CHANNEL_ID, "Ratspeak Background",
            NotificationManager.IMPORTANCE_LOW
        ).apply { description = "Keeps mesh network connections active" }
        getSystemService(NotificationManager::class.java).createNotificationChannel(channel)
    }

    private fun createMessageNotificationChannel() {
        val channel = NotificationChannel(
            MSG_CHANNEL_ID, "Messages",
            NotificationManager.IMPORTANCE_HIGH
        ).apply { description = "New message notifications" }
        getSystemService(NotificationManager::class.java).createNotificationChannel(channel)
    }

    private fun createCallNotificationChannel() {
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
            peerCount < 0 -> "Mesh network active"
            peerCount == 0 -> "Mesh active · no peers connected"
            peerCount == 1 -> "Mesh active · 1 peer connected"
            else -> "Mesh active · $peerCount peers connected"
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

    // Kept for a future foreground-card peer-count bridge.
    @Suppress("unused")
    private fun refreshForegroundNotification(peerCount: Int) {
        if (peerCount == lastKnownPeerCount) return
        lastKnownPeerCount = peerCount
        getSystemService(NotificationManager::class.java)
            .notify(NOTIFICATION_ID, buildNotification(peerCount))
    }

    /**
     * Legacy per-sender notification renderer retained in case Android needs
     * a service-local bridge again. Falls back to a single aggregate card if
     * the payload lacks breakdown info.
     *
     * Notification IDs are derived from a stable dest_hash hash so updates
     * land on the same card instead of stacking. A group summary is emitted
     * for Android N+ grouping behavior.
     */
    private fun reconcileMessageNotifications(senders: org.json.JSONArray?, totalCount: Int) {
        val nm = getSystemService(NotificationManager::class.java)

        if (senders == null || senders.length() == 0) {
            showAggregateNotification(totalCount)
            return
        }

        val currentHashes = HashSet<String>()
        val summaryLines = ArrayList<String>()

        for (i in 0 until senders.length()) {
            val s = senders.optJSONObject(i) ?: continue
            val hash = s.optString("hash", "")
            if (hash.isEmpty()) continue
            val name = s.optString("display_name", "").takeIf { it.isNotEmpty() }
                ?: "${hash.take(8)}…"
            val count = s.optInt("count", 0)
            val preview = s.optString("preview", "")
            if (count <= 0) continue

            currentHashes.add(hash)
            val id = perSenderId(hash)
            val prior = senderState[hash]?.second ?: 0
            // Only re-notify when the unread count for this sender actually increased,
            // to avoid repeatedly buzzing on the same message.
            val isNew = count > prior
            senderState[hash] = Pair(id, count)

            val title = if (count > 1) "$name · $count new" else name
            val body = preview.ifEmpty { "New message" }

            val openIntent = Intent(this, MainActivity::class.java).apply {
                flags = Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP
                putExtra("navigate_to", "message")
                putExtra("dest_hash", hash)
            }
            val pi = PendingIntent.getActivity(
                this, id, openIntent,
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
            )
            val builder = NotificationCompat.Builder(this, MSG_CHANNEL_ID)
                .setContentTitle(title)
                .setContentText(body)
                .setStyle(NotificationCompat.BigTextStyle().bigText(body))
                .setSmallIcon(R.drawable.ic_notification)
                .setPriority(NotificationCompat.PRIORITY_HIGH)
                .setAutoCancel(true)
                .setContentIntent(pi)
                .setGroup(MSG_NOTIFICATION_GROUP)
                .setOnlyAlertOnce(!isNew)
            nm.notify(id, builder.build())

            summaryLines.add("$name: $body")
        }

        // Cancel notifications for senders whose unread is now zero.
        val toRemove = senderState.keys.filter { it !in currentHashes }
        for (hash in toRemove) {
            senderState[hash]?.let { (id, _) -> nm.cancel(id) }
            senderState.remove(hash)
        }

        if (summaryLines.isEmpty()) {
            nm.cancel(GROUP_SUMMARY_ID)
            return
        }

        val summaryIntent = Intent(this, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP
            putExtra("navigate_to", "message")
        }
        val summaryPi = PendingIntent.getActivity(
            this, GROUP_SUMMARY_ID, summaryIntent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )
        val inbox = NotificationCompat.InboxStyle()
            .setBigContentTitle("$totalCount new message${if (totalCount == 1) "" else "s"}")
            .setSummaryText("Ratspeak")
        for (line in summaryLines.take(6)) inbox.addLine(line)
        if (summaryLines.size > 6) inbox.addLine("+${summaryLines.size - 6} more")

        val summary = NotificationCompat.Builder(this, MSG_CHANNEL_ID)
            .setContentTitle("Ratspeak")
            .setContentText("$totalCount new message${if (totalCount == 1) "" else "s"}")
            .setStyle(inbox)
            .setSmallIcon(R.drawable.ic_notification)
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setAutoCancel(true)
            .setContentIntent(summaryPi)
            .setGroup(MSG_NOTIFICATION_GROUP)
            .setGroupSummary(true)
            .build()
        nm.notify(GROUP_SUMMARY_ID, summary)
    }

    /** Fallback path when the backend doesn't provide a sender breakdown. */
    private fun showAggregateNotification(totalCount: Int) {
        val intent = Intent(this, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP
            putExtra("navigate_to", "message")
        }
        val pi = PendingIntent.getActivity(
            this, 1, intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )
        val text = if (totalCount == 1) "You received a message." else "$totalCount new messages"
        val notification = NotificationCompat.Builder(this, MSG_CHANNEL_ID)
            .setContentTitle("Ratspeak")
            .setContentText(text)
            .setSmallIcon(R.drawable.ic_notification)
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setAutoCancel(true)
            .setContentIntent(pi)
            .build()
        getSystemService(NotificationManager::class.java)
            .notify(GROUP_SUMMARY_ID, notification)
    }

    private fun clearAllMessageNotifications() {
        val nm = getSystemService(NotificationManager::class.java)
        for ((_, pair) in senderState) nm.cancel(pair.first)
        senderState.clear()
        nm.cancel(GROUP_SUMMARY_ID)
    }

    /**
     * Map a dest_hash (hex) to a stable notification ID in [PER_SENDER_ID_OFFSET, Int.MAX).
     * Uses the 32-bit FNV-1a hash of the hash string — collisions are tolerable
     * (two senders would briefly share a card) but cheap and deterministic.
     */
    private fun perSenderId(hash: String): Int {
        var h: Int = 0x811c9dc5.toInt()
        for (ch in hash) {
            h = h xor ch.code
            h *= 0x01000193
        }
        val bucket = (h ushr 1) % 1_000_000
        return PER_SENDER_ID_OFFSET + bucket
    }
}
