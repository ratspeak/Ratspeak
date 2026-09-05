package org.ratspeak.android

import android.app.Application
import android.app.Notification
import android.content.Context
import android.content.Intent
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith

/** No ActivityScenario: notification delivery must work with Application alone. */
@RunWith(AndroidJUnit4::class)
class RatspeakNotificationsTest {
    private val hash = "ab".repeat(16)
    private val context: Context get() = ApplicationProvider.getApplicationContext()

    private class RecordingDelivery(private val allowed: Boolean = true) : RatspeakNotifications.Delivery {
        var permissionChecks = 0
        var posted: Notification? = null
        var notificationId: Int? = null
        var sourceContext: Context? = null
        override fun permitted(context: Context): Boolean {
            permissionChecks++
            return allowed
        }
        override fun post(context: Context, id: Int, notification: Notification) {
            sourceContext = context
            notificationId = id
            posted = notification
        }
    }

    private fun payload(route: String = "lxmf:$hash", id: Int = 1_234): String = JSONObject()
        .put("title", "Message from Alice")
        .put("body", "Existing private preview")
        .put("route", route)
        .put("id", id)
        .put("channel", "ratspeak_messages")
        .toString()

    @Test fun applicationContextPostsWithoutAnActivityAndKeepsExistingPrivacyAndIds() {
        assertTrue(context is Application)
        val delivery = RecordingDelivery()
        assertEquals(RatspeakNotifications.SENT, RatspeakNotifications.deliver(context, payload(), delivery))
        assertSame(context, delivery.sourceContext)
        assertEquals(1_234, delivery.notificationId)
        val notification = delivery.posted!!
        assertEquals("Message from Alice", notification.extras.getString(Notification.EXTRA_TITLE))
        assertEquals("Existing private preview", notification.extras.getString(Notification.EXTRA_TEXT))
        assertEquals("lxmf:$hash", notification.group)
        assertEquals(Notification.VISIBILITY_PRIVATE, notification.visibility)
        assertTrue(notification.flags and Notification.FLAG_AUTO_CANCEL != 0)
        assertTrue(notification.flags and Notification.FLAG_ONLY_ALERT_ONCE != 0)
        assertNotNull(notification.contentIntent)
    }

    @Test fun deniedPermissionNeverPostsOrRequestsPermission() {
        val delivery = RecordingDelivery(allowed = false)
        assertEquals(RatspeakNotifications.PERMISSION_DENIED, RatspeakNotifications.deliver(context, payload(), delivery))
        assertEquals(1, delivery.permissionChecks)
        assertNull(delivery.posted)
    }

    @Test fun invalidOrOversizedPayloadNeverReachesThePlatform() {
        val delivery = RecordingDelivery()
        for (serialized in listOf(
            payload("javascript:alert(1)"),
            payload("channels:$hash:ff"),
            JSONObject(payload()).put("body", "x".repeat(2049)).toString(),
            JSONObject(payload()).put("channel", "unreviewed_channel").toString(),
            "x".repeat(16 * 1024 + 1),
        )) {
            assertEquals(RatspeakNotifications.INVALID_PAYLOAD, RatspeakNotifications.deliver(context, serialized, delivery))
        }
        assertEquals(0, delivery.permissionChecks)
        assertNull(delivery.posted)
    }

    @Test fun tapIntentIsExplicitAndOnlyAcceptsValidatedNotificationRoutes() {
        val intent = RatspeakNotifications.notificationIntent(context, "lxmf:$hash")
        assertEquals(MainActivity::class.java.name, intent.component?.className)
        assertEquals(RatspeakNotifications.ACTION_OPEN, intent.action)
        assertEquals("lxmf:$hash", RatspeakNotifications.routeFromIntent(intent))
        assertTrue(intent.flags and Intent.FLAG_ACTIVITY_SINGLE_TOP != 0)
        assertNull(RatspeakNotifications.routeFromIntent(Intent(intent).setAction(Intent.ACTION_VIEW)))
        assertNull(RatspeakNotifications.routeFromIntent(Intent(intent).putExtra(RatspeakNotifications.ROUTE_EXTRA, "lxmf:invalid")))
        assertNull(RatspeakNotifications.notificationIntent(context, null).getStringExtra(RatspeakNotifications.ROUTE_EXTRA))
    }

    @Test fun permissionRevocationDuringDeliveryReturnsAStableFailureCode() {
        val delivery = object : RatspeakNotifications.Delivery {
            override fun permitted(context: Context) = true
            override fun post(context: Context, id: Int, notification: Notification) {
                throw SecurityException("synthetic permission race")
            }
        }
        assertEquals(RatspeakNotifications.PERMISSION_DENIED, RatspeakNotifications.deliver(context, payload(), delivery))
    }
}
