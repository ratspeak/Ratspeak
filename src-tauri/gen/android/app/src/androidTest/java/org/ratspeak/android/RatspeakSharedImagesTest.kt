package org.ratspeak.android

import android.content.ClipData
import android.content.Context
import android.content.ContextWrapper
import android.content.Intent
import android.net.Uri
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import java.io.ByteArrayInputStream
import java.io.File
import java.io.IOException
import java.security.KeyStore
import java.util.UUID
import javax.crypto.KeyGenerator

@RunWith(AndroidJUnit4::class)
class RatspeakSharedImagesTest {
    private val id = "a".repeat(32)
    private val second = "b".repeat(32)
    private fun isolated(test: (RatspeakSharedImages, File) -> Unit) {
        val base = ApplicationProvider.getApplicationContext<Context>()
        val directory = File(base.cacheDir, "shared-image-test-${UUID.randomUUID()}")
        assertTrue(directory.mkdir())
        val alias = "ratspeak.image-test.${UUID.randomUUID()}"
        val key = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").apply {
            init(KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setKeySize(256).setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE).build())
        }.generateKey()
        val context = object : ContextWrapper(base) { override fun getNoBackupFilesDir() = directory }
        try { test(RatspeakSharedImages(context) { key }, directory) }
        finally {
            KeyStore.getInstance("AndroidKeyStore").apply { load(null); deleteEntry(alias) }
            assertTrue(directory.deleteRecursively())
        }
    }

    @Test fun oneContentUriOnlyAndClipFallback() {
        val uri = Uri.parse("content://synthetic.photos/one")
        val intent = Intent(Intent.ACTION_SEND).setType("image/jpeg").putExtra(Intent.EXTRA_STREAM, uri)
        assertEquals(uri, RatspeakSharedImages.imageUri(intent))
        intent.clipData = ClipData.newRawUri("photo", uri)
        assertEquals(uri, RatspeakSharedImages.imageUri(intent))
        intent.removeExtra(Intent.EXTRA_STREAM)
        assertEquals(uri, RatspeakSharedImages.imageUri(intent))
        intent.clipData!!.addItem(ClipData.Item(Uri.parse("content://synthetic.photos/two")))
        assertThrows(Exception::class.java) { RatspeakSharedImages.imageUri(intent) }
        intent.clipData = null
        intent.putExtra(Intent.EXTRA_STREAM, Uri.parse("file:///data/private"))
        assertThrows(Exception::class.java) { RatspeakSharedImages.imageUri(intent) }
        intent.putExtra(Intent.EXTRA_STREAM, uri).setAction(Intent.ACTION_SEND_MULTIPLE)
        assertThrows(Exception::class.java) { RatspeakSharedImages.imageUri(intent) }
    }

    @Test fun encryptedChunksAuthenticateIdentityOrderLengthAndBytes() = isolated { store, dir ->
        val source = ByteArray(RatspeakSharedImages.CHUNK + 39) { (it % 251).toByte() }
        assertEquals(source.size, store.seal(id, ByteArrayInputStream(source)))
        val file = File(dir, "shared-image-$id.sealed")
        assertFalse(file.readBytes().take(100) == source.take(100))
        assertArrayEquals(source.copyOfRange(0, RatspeakSharedImages.CHUNK), store.readChunk(id, 0, source.size.toLong()))
        assertArrayEquals(source.copyOfRange(RatspeakSharedImages.CHUNK, source.size), store.readChunk(id, 1, source.size.toLong()))
        assertThrows(Exception::class.java) { store.readChunk(id, 2, source.size.toLong()) }
        assertThrows(Exception::class.java) { store.readChunk(id, 0, source.size.toLong() - 1) }
        file.copyTo(File(dir, "shared-image-$second.sealed"))
        assertThrows(Exception::class.java) { store.readChunk(second, 0, source.size.toLong()) }
        val corrupt = file.readBytes(); corrupt[20] = (corrupt[20].toInt() xor 1).toByte(); file.writeBytes(corrupt)
        assertThrows(Exception::class.java) { store.readChunk(id, 0, source.size.toLong()) }
    }

    @Test fun failedCopiesDoNotReplaceCommittedImageAndCleanupIsExact() = isolated { store, dir ->
        val source = byteArrayOf(1, 2, 3)
        store.seal(id, ByteArrayInputStream(source))
        assertThrows(Exception::class.java) { store.seal(id, ByteArrayInputStream(ByteArray(10)), 9) }
        assertArrayEquals(source, store.readChunk(id, 0, 3))
        assertThrows(Exception::class.java) { store.seal(id, ByteArrayInputStream(byteArrayOf())) }
        assertArrayEquals(source, store.readChunk(id, 0, 3))
        assertThrows(IOException::class.java) { store.seal(id, ByteArrayInputStream(source)) { throw IOException("cancelled") } }
        assertArrayEquals(source, store.readChunk(id, 0, 3))
        store.seal(second, ByteArrayInputStream(source))
        File(dir, "unrelated").writeText("keep")
        store.prune(setOf(id))
        assertTrue(File(dir, "shared-image-$id.sealed").exists())
        assertFalse(File(dir, "shared-image-$second.sealed").exists())
        assertTrue(File(dir, "unrelated").exists())
        assertThrows(Exception::class.java) { store.copy(second, "content://ungranted.example/photo", "image/jpeg") }
        assertThrows(Exception::class.java) { store.copy(second, "file:///data/private", "image/jpeg") }
        assertThrows(Exception::class.java) { store.copy(second, "content://org.ratspeak.android.fileprovider/private", "image/jpeg") }
        assertThrows(Exception::class.java) { store.copy(second, "content://0@org.ratspeak.android.fileprovider/private", "image/jpeg") }
    }
}
