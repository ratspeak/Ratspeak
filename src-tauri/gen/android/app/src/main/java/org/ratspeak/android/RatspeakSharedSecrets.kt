package org.ratspeak.android

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/** RPC keys encrypted by a non-exportable, app-owned Android Keystore key. */
object RatspeakSharedSecrets {
    private const val ALIAS = "ratspeak.shared-instance.v1"
    private const val PREFS = "shared_instance_credentials"

    @Synchronized
    private fun key(create: Boolean): SecretKey {
        val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (store.getKey(ALIAS, null) as? SecretKey)?.let { return it }
        check(create) { "Credential encryption key unavailable" }
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").apply {
            init(KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setKeySize(256)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setRandomizedEncryptionRequired(true)
                .build())
        }.generateKey()
    }

    @JvmStatic
    @Synchronized
    fun write(context: Context, id: String, secret: ByteArray) {
        require(secret.size in 1..1024)
        try {
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(Cipher.ENCRYPT_MODE, key(true))
            cipher.updateAAD(id.toByteArray(Charsets.UTF_8))
            val sealed = cipher.iv + cipher.doFinal(secret)
            check(context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit()
                .putString(id, Base64.encodeToString(sealed, Base64.NO_WRAP)).commit())
        } finally {
            secret.fill(0)
        }
    }

    @JvmStatic
    @Synchronized
    fun read(context: Context, id: String): ByteArray {
        val encoded = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString(id, null)
            ?: error("Credential unavailable")
        require(encoded.length <= 2048)
        val sealed = Base64.decode(encoded, Base64.NO_WRAP)
        require(sealed.size in 29..1052)
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.DECRYPT_MODE, key(false), GCMParameterSpec(128, sealed.copyOfRange(0, 12)))
        cipher.updateAAD(id.toByteArray(Charsets.UTF_8))
        return cipher.doFinal(sealed, 12, sealed.size - 12)
    }

    @JvmStatic
    @Synchronized
    fun delete(context: Context, id: String) {
        check(context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit().remove(id).commit())
    }
}
