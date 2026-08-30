package org.ratspeak.android

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import java.util.UUID

/** Exercises the real Android Keystore; never reads or changes app identities. */
@RunWith(AndroidJUnit4::class)
class RatspeakSharedSecretsTest {
    @Test fun encryptedRoundTripAndDeletion() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val id = "shared-test-${UUID.randomUUID()}"
        val secret = ByteArray(17) { (it + 1).toByte() }
        val expected = secret.copyOf()
        try {
            RatspeakSharedSecrets.write(context, id, secret)
            assertTrue(secret.all { it == 0.toByte() })
            assertArrayEquals(expected, RatspeakSharedSecrets.read(context, id))
            val stored = context.getSharedPreferences("shared_instance_credentials", Context.MODE_PRIVATE).getString(id, null)!!
            assertNotEquals(android.util.Base64.encodeToString(expected, android.util.Base64.NO_WRAP), stored)
            RatspeakSharedSecrets.delete(context, id)
            assertThrows(IllegalStateException::class.java) { RatspeakSharedSecrets.read(context, id) }
        } finally { RatspeakSharedSecrets.delete(context, id); expected.fill(0) }
    }

    @Test fun ciphertextCannotBeReassignedToAnotherCredential() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val first = "shared-test-${UUID.randomUUID()}"
        val second = "shared-test-${UUID.randomUUID()}"
        try {
            RatspeakSharedSecrets.write(context, first, byteArrayOf(1, 2, 3))
            val prefs = context.getSharedPreferences("shared_instance_credentials", Context.MODE_PRIVATE)
            assertTrue(prefs.edit().putString(second, prefs.getString(first, null)).commit())
            assertThrows(Exception::class.java) { RatspeakSharedSecrets.read(context, second) }
        } finally { RatspeakSharedSecrets.delete(context, first); RatspeakSharedSecrets.delete(context, second) }
    }
}
