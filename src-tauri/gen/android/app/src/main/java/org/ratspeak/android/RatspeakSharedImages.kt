package org.ratspeak.android

import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.CancellationSignal
import android.os.Process
import android.provider.OpenableColumns
import android.util.AtomicFile
import androidx.core.net.toUri
import org.json.JSONObject
import java.io.File
import java.io.InputStream
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import javax.crypto.Cipher
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/** Native-only, bounded photo copies. Neither URIs nor source bytes enter the WebView.
 * Each fixed-size chunk uses standard AES-GCM with its item/index as AAD. The
 * encrypted inbox authenticates total length; reading verifies exact file length
 * and every chunk before the existing Rust image pipeline can inspect it.
 */
internal class RatspeakSharedImages(private val context: Context, private val key: (Boolean) -> SecretKey) {
    companion object {
        const val CHUNK = 256 * 1024
        const val MAX_BYTES = 128_000_000
        private val deadline = Executors.newSingleThreadScheduledExecutor()
        fun imageUri(intent: Intent): Uri {
            require(intent.action == Intent.ACTION_SEND && intent.type?.startsWith("image/") == true)
            require(intent.clipData == null || intent.clipData!!.itemCount == 1) { "Share one photo at a time." }
            @Suppress("DEPRECATION")
            val extra = intent.getParcelableExtra<android.os.Parcelable>(Intent.EXTRA_STREAM)
            require(extra == null || extra is Uri)
            val clip = intent.clipData?.getItemAt(0)?.uri
            val uri = (extra as? Uri) ?: clip ?: error("No shared photo found.")
            require(clip == null || clip == uri) { "Share one photo at a time." }
            require(uri.scheme == "content" && !uri.authority.isNullOrEmpty())
            return uri
        }
    }

    private fun file(id: String): AtomicFile {
        require(id.matches(Regex("[0-9a-f]{32}")))
        return AtomicFile(File(context.noBackupFilesDir, "shared-image-$id.sealed"))
    }
    private fun aad(id: String, index: Int) = "ratspeak.shared-image.v1/$id/$index".toByteArray(Charsets.UTF_8)

    fun copy(id: String, uriText: String, suppliedMime: String): String {
        val uri = uriText.toUri()
        require(uri.scheme == "content" && !uri.authority.isNullOrEmpty())
        // A forged Intent must not turn this app's own file provider or ambient
        // storage permissions into a way of extracting private files.
        require(context.checkUriPermission(uri, Process.myPid(), Process.myUid(), Intent.FLAG_GRANT_READ_URI_PERMISSION) == PackageManager.PERMISSION_GRANTED)
        // Work-profile URIs may prefix the authority with a user ID. Resolve
        // the underlying provider for the own-UID check, not that decorated name.
        val provider = context.packageManager.resolveContentProvider(uri.authority!!.substringAfterLast('@'), 0)
        require(provider == null || provider.applicationInfo.uid != Process.myUid())
        val resolver = context.contentResolver
        val cancellation = CancellationSignal()
        val timeout = deadline.schedule({ cancellation.cancel() }, 30, TimeUnit.SECONDS)
        try {
            var name = "photo"
            resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null, cancellation)?.use { cursor ->
                if (cursor.moveToFirst()) {
                    name = cursor.getString(0)?.filterNot { it.isISOControl() || it == '/' || it == '\\' }?.take(60)?.trim().orEmpty().ifEmpty { "photo" }
                }
            }
            val mime = suppliedMime.takeIf { it.matches(Regex("image/[a-zA-Z0-9.+_*-]{1,80}")) } ?: "image/*"
            val asset = resolver.openAssetFileDescriptor(uri, "r", cancellation) ?: error("Photo unavailable")
            asset.use {
                val stream = it.createInputStream()
                val closeTimeout = deadline.schedule({ try { stream.close() } catch (_: Exception) {} }, 30, TimeUnit.SECONDS)
                try {
                    val size = stream.use { input -> seal(id, input) { cancellation.throwIfCanceled() } }
                    return JSONObject().put("name", name).put("mime", mime).put("size", size).toString()
                } finally { closeTimeout.cancel(false) }
            }
        } finally { timeout.cancel(false) }
    }

    internal fun seal(id: String, input: InputStream, maxBytes: Int = MAX_BYTES, check: () -> Unit = {}): Int {
        require(maxBytes in 1..MAX_BYTES)
        val destination = file(id)
        val output = destination.startWrite()
        val buffer = ByteArray(CHUNK)
        var total = 0
        var index = 0
        try {
            val encryptionKey = key(true)
            while (true) {
                var count = 0
                while (count < buffer.size) {
                    check()
                    val read = input.read(buffer, count, buffer.size - count)
                    if (read < 0) break
                    require(read > 0) { "Photo provider stopped responding" }
                    count += read
                    require(total.toLong() + count <= maxBytes) { "Shared photo exceeds 128 MB" }
                }
                if (count == 0) break
                val cipher = Cipher.getInstance("AES/GCM/NoPadding")
                cipher.init(Cipher.ENCRYPT_MODE, encryptionKey)
                cipher.updateAAD(aad(id, index++))
                output.write(cipher.iv)
                output.write(cipher.doFinal(buffer, 0, count))
                total += count
                if (count < CHUNK) break
            }
            require(total > 0) { "Shared photo is empty" }
            check()
            output.fd.sync()
            destination.finishWrite(output)
            require(destination.baseFile.length() == total.toLong() + index * 28L)
            return total
        } catch (error: Exception) {
            destination.failWrite(output)
            throw error
        } finally { buffer.fill(0) }
    }

    fun readChunk(id: String, index: Int, size: Long): ByteArray {
        require(size in 1..MAX_BYTES.toLong() && index >= 0 && index.toLong() * CHUNK < size)
        val count = minOf(CHUNK.toLong(), size - index.toLong() * CHUNK).toInt()
        return file(id).openRead().use { input ->
            require(input.channel.size() == size + ((size + CHUNK - 1) / CHUNK) * 28)
            input.channel.position(index.toLong() * (CHUNK + 28))
            val sealed = ByteArray(count + 28)
            var offset = 0
            while (offset < sealed.size) {
                val n = input.read(sealed, offset, sealed.size - offset)
                require(n > 0)
                offset += n
            }
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(Cipher.DECRYPT_MODE, key(false), GCMParameterSpec(128, sealed.copyOfRange(0, 12)))
            cipher.updateAAD(aad(id, index))
            cipher.doFinal(sealed, 12, sealed.size - 12)
        }
    }

    fun prune(keep: Set<String>) {
        // Only this exact format is ours. Keep AtomicFile backups for live items.
        val pattern = Regex("shared-image-([0-9a-f]{32})\\.sealed(?:\\.bak|\\.new)?")
        context.noBackupFilesDir.listFiles()?.forEach { candidate ->
            val id = pattern.matchEntire(candidate.name)?.groupValues?.get(1)
            if (id != null && id !in keep) check(candidate.delete()) { "Shared photo cleanup failed" }
        }
    }
}
