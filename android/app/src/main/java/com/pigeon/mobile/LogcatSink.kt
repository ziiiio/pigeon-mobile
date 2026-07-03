package com.pigeon.mobile

import android.util.Log
import uniffi.pigeon_mobile_core.LogLevel
import uniffi.pigeon_mobile_core.LogSink

/** Forwards the core's log records to Android Logcat (M0.7). */
class LogcatSink : LogSink {
    override fun log(level: LogLevel, target: String, message: String) {
        val tag = "pigeon/$target"
        when (level) {
            LogLevel.ERROR -> Log.e(tag, message)
            LogLevel.WARN -> Log.w(tag, message)
            LogLevel.INFO -> Log.i(tag, message)
            LogLevel.DEBUG -> Log.d(tag, message)
            LogLevel.TRACE -> Log.v(tag, message)
        }
    }
}
