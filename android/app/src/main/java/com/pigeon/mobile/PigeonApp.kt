package com.pigeon.mobile

import android.app.Application
import uniffi.pigeon_mobile_core.emitTestLog
import uniffi.pigeon_mobile_core.setKeyStore
import uniffi.pigeon_mobile_core.setLogSink
import uniffi.pigeon_mobile_core.setStoreDir

/**
 * Installs the core's host callbacks exactly once, at process start: the Logcat
 * log sink (M0.7), the Android Keystore-backed key store (M1.3), and the local
 * store directory (M2.1/M2.2). Everything downstream — session restore, login,
 * persistence, sync — depends on these being in place, so they belong here
 * rather than in an Activity.
 */
class PigeonApp : Application() {
    override fun onCreate() {
        super.onCreate()
        setLogSink(LogcatSink())
        setKeyStore(AndroidKeyStore(applicationContext))
        // The SQLite store lives in the app's private files dir. Secrets never
        // land here — only rooms/timeline/state; the token stays in the keystore.
        setStoreDir(filesDir.absolutePath)
        emitTestLog("PigeonApp: core callbacks installed")
    }
}
