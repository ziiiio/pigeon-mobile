package com.pigeon.mobile

import android.app.Application
import uniffi.pigeon_mobile_core.emitTestLog
import uniffi.pigeon_mobile_core.setKeyStore
import uniffi.pigeon_mobile_core.setLogSink

/**
 * Installs the core's host callbacks exactly once, at process start: the Logcat
 * log sink (M0.7) and the Android Keystore-backed key store (M1.3). Everything
 * downstream — session restore, login, persistence — depends on these being in
 * place, so they belong here rather than in an Activity.
 */
class PigeonApp : Application() {
    override fun onCreate() {
        super.onCreate()
        setLogSink(LogcatSink())
        setKeyStore(AndroidKeyStore(applicationContext))
        emitTestLog("PigeonApp: core callbacks installed")
    }
}
