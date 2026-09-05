package org.ratspeak.android

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class RatspeakNotificationsPolicyTest {
    private val hash = "ab".repeat(16)

    @Test fun routesUseCanonicalProtocolIdentifiers() {
        assertTrue(RatspeakNotifications.validRoute("lxmf:$hash"))
        assertTrue(RatspeakNotifications.validRoute("lxst:$hash"))
        assertTrue(RatspeakNotifications.validRoute("lrgp:0123456789abcdef"))
        assertFalse(RatspeakNotifications.validRoute("lxmf:${hash.uppercase()}"))
        assertFalse(RatspeakNotifications.validRoute("lrgp:game-7"))
        assertFalse(RatspeakNotifications.validRoute("https://example.invalid"))
        assertFalse(RatspeakNotifications.validRoute("lxmf:$hash:extra"))
    }

    @Test fun channelRoutesRejectInvalidUtf8ControlsWhitespaceAndOversizeRooms() {
        assertTrue(RatspeakNotifications.validRoute("channels:$hash:6669656c6420636166c3a9"))
        for (room in listOf("ff", "0", "0a", "7f", "2061", "6120", "efbbbf61", "61efbbbf", "61".repeat(257))) {
            assertFalse(room, RatspeakNotifications.validRoute("channels:$hash:$room"))
        }
        assertTrue(RatspeakNotifications.validRoute("channels:$hash:${"61".repeat(256)}"))
        assertTrue(RatspeakNotifications.validRoute("channels:$hash:c28561"))
    }
}
