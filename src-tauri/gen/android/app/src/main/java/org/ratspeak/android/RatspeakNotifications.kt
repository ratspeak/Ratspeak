package org.ratspeak.android

import android.Manifest
import android.annotation.SuppressLint
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import org.json.JSONObject
import java.nio.ByteBuffer
import java.nio.charset.CodingErrorAction
import java.util.concurrent.atomic.AtomicInteger

/** Application-context delivery for the existing Rust notification policy. */
object RatspeakNotifications {
    internal const val ACTION_OPEN = "org.ratspeak.android.OPEN_NOTIFICATION"
    internal const val ROUTE_EXTRA = "notification_route"
    internal const val SENT = 0
    internal const val PERMISSION_DENIED = 1
    internal const val INVALID_PAYLOAD = 2
    internal const val DELIVERY_FAILED = 3
    private const val MAX_PAYLOAD_CHARS = 16 * 1024
    private val generatedId = AtomicInteger(5_000_000)
    private val hashPattern = Regex("^[0-9a-f]{32}$")
    private val sessionPattern = Regex("^[0-9a-f]{16}$")
    private val roomPattern = Regex("^(?:[0-9a-f]{2}){1,256}$")

    @JvmStatic
    fun initialize(context: Context) {
        try {
            // The pre-super Activity bootstrap can precede System.loadLibrary.
            // MainActivity repeats this after super.onCreate. Native OnceLock
            // makes subsequent Activity/service initialization harmless.
            nativeInitialize(context.applicationContext)
        } catch (_: UnsatisfiedLinkError) {
            // No fallback through Wry or an Activity-owned plugin.
        }
    }

    @JvmStatic
    private external fun nativeInitialize(context: Context)

    @JvmStatic
    fun show(context: Context, serialized: String): Int =
        deliver(context.applicationContext, serialized, AndroidDelivery)

    internal data class Payload(
        val title: String,
        val body: String,
        val route: String?,
        val id: Int?,
        val channel: String,
    )

    internal interface Delivery {
        fun permitted(context: Context): Boolean
        fun post(context: Context, id: Int, notification: Notification)
    }

    private object AndroidDelivery : Delivery {
        override fun permitted(context: Context): Boolean =
            (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
                ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) ==
                PackageManager.PERMISSION_GRANTED) &&
                NotificationManagerCompat.from(context).areNotificationsEnabled()

        @SuppressLint("MissingPermission")
        override fun post(context: Context, id: Int, notification: Notification) {
            // deliver checks runtime permission first. Revocation between this
            // check and Binder delivery is handled by its SecurityException path.
            NotificationManagerCompat.from(context).notify(id, notification)
        }
    }

    internal fun deliver(context: Context, serialized: String, delivery: Delivery): Int {
        val payload = parsePayload(serialized) ?: return INVALID_PAYLOAD
        return try {
            if (!delivery.permitted(context)) return PERMISSION_DENIED
            ensureChannel(context, payload.channel)
            val id = payload.id ?: generatedId.getAndUpdate { current ->
                if (current == Int.MAX_VALUE) 5_000_000 else current + 1
            }
            val intent = notificationIntent(context, payload.route)
            val pendingIntent = PendingIntent.getActivity(
                context, id, intent,
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
            )
            val builder = NotificationCompat.Builder(context, payload.channel)
                .setContentTitle(payload.title)
                .setContentText(payload.body)
                .setAutoCancel(true)
                .setOngoing(false)
                .setPriority(NotificationCompat.PRIORITY_DEFAULT)
                .setDefaults(Notification.DEFAULT_ALL)
                .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
                .setOnlyAlertOnce(true)
                .setSmallIcon(R.drawable.ic_notification)
                .setContentIntent(pendingIntent)
            payload.route?.let { builder.setGroup(it) }
            delivery.post(context, id, builder.build())
            SENT
        } catch (_: SecurityException) {
            PERMISSION_DENIED
        } catch (_: Exception) {
            // Rust logs only the stable result code, never title/body/route.
            DELIVERY_FAILED
        }
    }

    private fun ensureChannel(context: Context, id: String) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        val manager = context.getSystemService(NotificationManager::class.java)
        if (manager.getNotificationChannel(id) != null) return
        val calls = id == RatspeakService.CALL_CHANNEL_ID
        val channel = NotificationChannel(
            id, if (calls) "Calls" else "Messages", NotificationManager.IMPORTANCE_HIGH,
        ).apply {
            description = if (calls) "Incoming call notifications" else "New message notifications"
            if (calls) {
                enableVibration(true)
                lockscreenVisibility = Notification.VISIBILITY_PUBLIC
            }
        }
        manager.createNotificationChannel(channel)
    }

    internal fun notificationIntent(context: Context, route: String?): Intent {
        require(route == null || validRoute(route))
        return Intent(context, MainActivity::class.java).apply {
            action = ACTION_OPEN
            flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP or
                Intent.FLAG_ACTIVITY_SINGLE_TOP
            if (route != null) putExtra(ROUTE_EXTRA, route)
        }
    }

    internal fun routeFromIntent(intent: Intent?): String? {
        if (intent?.action != ACTION_OPEN) return null
        return try {
            intent.getStringExtra(ROUTE_EXTRA)?.takeIf(::validRoute)
        } catch (_: Exception) {
            null
        }
    }

    internal fun parsePayload(serialized: String): Payload? {
        if (serialized.length > MAX_PAYLOAD_CHARS) return null
        return try {
            val json = JSONObject(serialized)
            val title = json.opt("title") as? String ?: return null
            val body = json.opt("body") as? String ?: return null
            val channel = json.opt("channel") as? String ?: return null
            if (title.codePointCount(0, title.length) > 512 ||
                body.codePointCount(0, body.length) > 2048 ||
                channel !in setOf(RatspeakService.MSG_CHANNEL_ID, RatspeakService.CALL_CHANNEL_ID)
            ) return null
            val route = if (json.isNull("route")) null else json.opt("route") as? String ?: return null
            if (route != null && !validRoute(route)) return null
            val id = if (json.isNull("id")) null else json.opt("id") as? Int ?: return null
            Payload(title, body, route, id, channel)
        } catch (_: Exception) {
            null
        }
    }

    internal fun validRoute(route: String): Boolean {
        if (route.length > 554) return false
        val kind = route.substringBefore(':', "")
        val value = route.substringAfter(':', "")
        return when (kind) {
            "lxmf", "lxst" -> hashPattern.matches(value)
            "lrgp" -> sessionPattern.matches(value)
            "channels" -> {
                val hub = value.substringBefore(':', "")
                val encoded = value.substringAfter(':', "")
                if (!hashPattern.matches(hub) || !roomPattern.matches(encoded)) return false
                val bytes = ByteArray(encoded.length / 2) { index ->
                    encoded.substring(index * 2, index * 2 + 2).toInt(16).toByte()
                }
                val room = try {
                    Charsets.UTF_8.newDecoder()
                        .onMalformedInput(CodingErrorAction.REPORT)
                        .onUnmappableCharacter(CodingErrorAction.REPORT)
                        .decode(ByteBuffer.wrap(bytes)).toString()
                } catch (_: Exception) {
                    return false
                }
                room.isNotEmpty() && room == room.trim(::isRouteWhitespace) &&
                    room.none { it.code <= 0x1f || it.code == 0x7f }
            }
            else -> false
        }
    }

    // Keep routing identical to the frontend's ECMAScript String.trim rather
    // than Java/Kotlin's different Unicode whitespace definitions.
    private fun isRouteWhitespace(character: Char): Boolean = when (character.code) {
        in 0x09..0x0d, 0x20, 0xa0, 0x1680, in 0x2000..0x200a,
        0x2028, 0x2029, 0x202f, 0x205f, 0x3000, 0xfeff -> true
        else -> false
    }
}
