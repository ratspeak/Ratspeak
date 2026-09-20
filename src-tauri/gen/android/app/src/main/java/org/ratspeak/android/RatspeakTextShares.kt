package org.ratspeak.android

import android.content.Context
import android.content.Intent
import android.os.Handler
import android.os.Looper
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.AtomicFile
import android.widget.Toast
import java.io.ByteArrayOutputStream
import java.io.File
import java.security.KeyStore
import java.util.UUID
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import org.json.JSONArray

/** Only OS intake and encrypted, app-private storage. Rust owns queue policy. */
object RatspeakTextShares {
    private const val MAX_BYTES = 4 * 1024 * 1024
    private const val KEY = "ratspeak.text-shares.v1"
    private const val FILE = "text-shares-v1.sealed"
    private val aad = "ratspeak.text-shares.v1".toByteArray(Charsets.UTF_8)
    private var appContext: Context? = null
    private val main = Handler(Looper.getMainLooper())
    private val worker = ThreadPoolExecutor(1, 1, 0, TimeUnit.SECONDS, ArrayBlockingQueue<Runnable>(8))

    @Synchronized
    fun initialize(context: Context) { appContext = context.applicationContext }

    private fun context(): Context = checkNotNull(appContext) { "Text sharing unavailable" }
    private fun file(): AtomicFile = AtomicFile(File(context().noBackupFilesDir, FILE))

    // Only text or one image. Never coerce arbitrary content URIs to text.
    fun receive(intent: Intent?, restoredId: String? = null): String? {
        if (intent?.action == Intent.ACTION_SEND_MULTIPLE) {
            showError("Share one photo at a time.")
            return null
        }
        if (intent?.action != Intent.ACTION_SEND) return null
        val id = restoredId?.takeIf { it.matches(Regex("[0-9a-f]{32}")) }
            ?: UUID.randomUUID().toString().replace("-", "")
        try {
            val image = intent.type?.startsWith("image/") == true
            require(intent.type == "text/plain" || image)
            val uri = if (image) RatspeakSharedImages.imageUri(intent).toString() else null
            val extra = intent.getCharSequenceExtra(Intent.EXTRA_TEXT)
                ?: intent.clipData?.takeIf { it.itemCount == 1 }?.getItemAt(0)?.text
            require((image || extra != null) && (extra?.length ?: 0) <= 65536)
            val subject = intent.getCharSequenceExtra(Intent.EXTRA_SUBJECT)
            require(subject == null || subject.length <= 2048)
            val text = extra?.toString() ?: ""
            val title = subject?.toString() ?: ""
            require(text.toByteArray(Charsets.UTF_8).size <= 65536)
            worker.execute {
                val error = try { if (uri != null) acceptImage(id, text, title, uri, intent.type ?: "image/*") else accept(id, text, title) }
                    catch (_: Exception) { "Could not save this share. Open Ratspeak and try sharing again." }
                    catch (_: UnsatisfiedLinkError) { "Ratspeak is still starting. Try sharing again." }
                if (error.isNotEmpty()) showError(error)
                else main.post {
                    // Stop retaining raw text in this Activity's launch Intent
                    // after durable acceptance. A failed write retains retry data.
                    intent.removeExtra(Intent.EXTRA_TEXT)
                    intent.removeExtra(Intent.EXTRA_SUBJECT)
                    intent.removeExtra(Intent.EXTRA_HTML_TEXT)
                    intent.removeExtra(Intent.EXTRA_STREAM)
                    intent.clipData = null
                    intent.action = Intent.ACTION_MAIN
                }
            }
        } catch (_: Exception) {
            showError("Share text or a link up to 64 KiB, or one photo up to 128 MB, then try again.")
        }
        // Saved-state ID belongs to this Activity delivery, never to an extra
        // supplied by another app. Deliberate onNewIntent shares get fresh IDs.
        return id
    }

    private fun showError(message: String) {
        main.post { Toast.makeText(context(), message, Toast.LENGTH_LONG).show() }
    }

    private fun key(create: Boolean): SecretKey {
        val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (store.getKey(KEY, null) as? SecretKey)?.let { return it }
        check(create) { "Shared draft encryption key unavailable" }
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").apply {
            init(KeyGenParameterSpec.Builder(KEY, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setKeySize(256).setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setRandomizedEncryptionRequired(true).build())
        }.generateKey()
    }

    @JvmStatic
    @Synchronized
    fun read(): String {
        val file = file()
        if (!file.baseFile.exists() && !File(file.baseFile.path + ".bak").exists()) return ""
        val sealed = readSealed(file)
        require(sealed.size >= 28)
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.DECRYPT_MODE, key(false), GCMParameterSpec(128, sealed.copyOfRange(0, 12)))
        cipher.updateAAD(aad)
        val plain = cipher.doFinal(sealed, 12, sealed.size - 12)
        return try { plain.toString(Charsets.UTF_8) } finally { plain.fill(0) }
    }

    private fun readSealed(file: AtomicFile): ByteArray = file.openRead().use { input ->
            val output = ByteArrayOutputStream()
            val buffer = ByteArray(8192)
            while (true) {
                val size = input.read(buffer)
                if (size < 0) break
                check(output.size() + size <= MAX_BYTES + 28)
                output.write(buffer, 0, size)
            }
            output.toByteArray()
    }

    @JvmStatic
    @Synchronized
    fun write(value: String) {
        val plain = value.toByteArray(Charsets.UTF_8)
        try {
            require(plain.size <= MAX_BYTES)
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(Cipher.ENCRYPT_MODE, key(true))
            cipher.updateAAD(aad)
            val sealed = cipher.iv + cipher.doFinal(plain)
            val file = file()
            val output = file.startWrite()
            try {
                output.write(sealed)
                // AtomicFile logs some sync/rename failures instead of throwing.
                // Require an explicit sync and verified commit before accepting.
                output.fd.sync()
                file.finishWrite(output)
                check(readSealed(file).contentEquals(sealed)) { "Shared draft commit failed" }
            } catch (error: Exception) {
                file.failWrite(output)
                throw error
            }
        } finally { plain.fill(0) }
    }

    @JvmStatic
    private external fun accept(id: String, text: String, subject: String): String

    @JvmStatic
    private external fun acceptImage(id: String, text: String, subject: String, uri: String, mime: String): String

    @JvmStatic
    fun copyImage(id: String, uri: String, mime: String): String = RatspeakSharedImages(context(), ::key).copy(id, uri, mime)

    @JvmStatic
    fun readImageChunk(id: String, index: Int, size: Long): ByteArray = RatspeakSharedImages(context(), ::key).readChunk(id, index, size)

    @JvmStatic
    fun pruneImages(ids: String) {
        val values = JSONArray(ids)
        RatspeakSharedImages(context(), ::key).prune((0 until values.length()).map { values.getString(it) }.toSet())
    }
}
