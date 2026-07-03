package com.pigeon.mobile

import android.content.Context
import android.content.SharedPreferences
import android.util.Base64
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import uniffi.pigeon_mobile_core.KeyStore
import uniffi.pigeon_mobile_core.KeyStoreException

/**
 * The core's [KeyStore], backed by [EncryptedSharedPreferences] — values are
 * encrypted at rest with a master key held in the Android Keystore.
 *
 * This is where the session token lives on device (CLAUDE.md Gotcha #1). The
 * Rust core references it through this callback and hands us opaque bytes; the
 * app never inspects or holds the token itself. Failures are surfaced to the
 * core as [KeyStoreException] (→ `CoreError.Storage`).
 */
class AndroidKeyStore(context: Context) : KeyStore {

    private val prefs: SharedPreferences = run {
        val masterKey = MasterKey.Builder(context)
            .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
            .build()
        EncryptedSharedPreferences.create(
            context,
            PREFS_NAME,
            masterKey,
            EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
            EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
        )
    }

    override fun put(key: String, value: ByteArray) {
        try {
            // EncryptedSharedPreferences stores strings; Base64 the raw bytes.
            prefs.edit().putString(key, Base64.encodeToString(value, Base64.NO_WRAP)).apply()
        } catch (e: Exception) {
            throw KeyStoreException.Backend("put failed: ${e.message}")
        }
    }

    override fun get(key: String): ByteArray? {
        return try {
            prefs.getString(key, null)?.let { Base64.decode(it, Base64.NO_WRAP) }
        } catch (e: Exception) {
            throw KeyStoreException.Backend("get failed: ${e.message}")
        }
    }

    override fun delete(key: String) {
        try {
            prefs.edit().remove(key).apply()
        } catch (e: Exception) {
            throw KeyStoreException.Backend("delete failed: ${e.message}")
        }
    }

    private companion object {
        const val PREFS_NAME = "pigeon_secure_prefs"
    }
}
