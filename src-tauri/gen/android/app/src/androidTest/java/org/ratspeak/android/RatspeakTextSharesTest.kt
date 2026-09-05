package org.ratspeak.android

import android.content.Context
import android.content.ContextWrapper
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.util.UUID

/** Real Keystore/AtomicFile checks in a unique test directory, never app drafts. */
@RunWith(AndroidJUnit4::class)
class RatspeakTextSharesTest {
    private fun isolated(check: (Context, File) -> Unit) {
        val base = ApplicationProvider.getApplicationContext<Context>()
        val directory = File(base.cacheDir, "shared-text-test-${UUID.randomUUID()}")
        assertTrue(directory.mkdir())
        val context = object : ContextWrapper(base) {
            override fun getApplicationContext(): Context = this
            override fun getNoBackupFilesDir(): File = directory
        }
        try {
            RatspeakTextShares.initialize(context)
            check(context, File(directory, "text-shares-v1.sealed"))
        } finally {
            RatspeakTextShares.initialize(base)
            assertTrue(directory.deleteRecursively())
        }
    }

    @Test fun encryptedRoundTripAndUnavailableCorruptData() = isolated { context, file ->
        val text = "001234 Привет 👋 https://ozon.example/001?q=2"
        assertEquals("", RatspeakTextShares.read())
        RatspeakTextShares.write(text)
        val original = file.readBytes()
        assertFalse(original.toString(Charsets.UTF_8).contains(text))
        RatspeakTextShares.initialize(context)
        assertEquals(text, RatspeakTextShares.read())
        val changed = original.copyOf()
        changed[changed.lastIndex] = (changed.last().toInt() xor 1).toByte()
        file.writeBytes(changed)
        assertThrows(Exception::class.java) { RatspeakTextShares.read() }
        file.writeBytes(original)
        assertEquals(text, RatspeakTextShares.read())
    }

    @Test fun rejectedOversizeWritePreservesCommittedData() = isolated { _, _ ->
        RatspeakTextShares.write("kept")
        assertThrows(IllegalArgumentException::class.java) {
            RatspeakTextShares.write("x".repeat(4 * 1024 * 1024 + 1))
        }
        assertEquals("kept", RatspeakTextShares.read())
    }

    @Test fun interruptedAtomicWriteRecoversPreviousValue() = isolated { _, file ->
        RatspeakTextShares.write("committed")
        // AtomicFile still recovers the legacy backup format after an upgrade.
        val backup = File(file.path + ".bak")
        assertTrue(file.renameTo(backup))
        file.writeBytes(byteArrayOf(1, 2, 3))
        assertEquals("committed", RatspeakTextShares.read())
        assertFalse(backup.exists())
    }
}
