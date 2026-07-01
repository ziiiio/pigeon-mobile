package com.pigeon.mobile

import android.os.Bundle
import android.util.Log
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import uniffi.pigeon_mobile_core.CoreException
import uniffi.pigeon_mobile_core.LogLevel
import uniffi.pigeon_mobile_core.LogSink
import uniffi.pigeon_mobile_core.coreVersion
import uniffi.pigeon_mobile_core.emitTestLog
import uniffi.pigeon_mobile_core.selfTestCrypto
import uniffi.pigeon_mobile_core.setLogSink

/**
 * Hello-core (M0.4). Proves the full pipeline round-trips: a value computed in
 * the Rust core, surfaced through the generated UniFFI bindings, rendered by
 * Compose — plus the host log sink (M0.7) forwarding core logs to Logcat.
 *
 * This is toolchain validation only; real screens arrive in M1+.
 */
class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        // Install the host log sink and prove the callback round-trips (M0.7).
        setLogSink(LogcatSink())
        emitTestLog("Hello-core: log sink installed")

        val version = coreVersion()
        val cryptoStatus = try {
            "${selfTestCrypto("@hello:pigeon.local")} bytes"
        } catch (e: CoreException) {
            "error: ${e.message}"
        }

        setContent {
            MaterialTheme {
                Surface(modifier = Modifier.fillMaxSize()) {
                    HelloCore(version, cryptoStatus)
                }
            }
        }
    }
}

/** Forwards core log records to Android Logcat. */
private class LogcatSink : LogSink {
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

@Composable
private fun HelloCore(version: String, cryptoStatus: String) {
    Column(modifier = Modifier.padding(24.dp)) {
        Text(text = version, style = MaterialTheme.typography.titleMedium)
        Spacer(Modifier.height(12.dp))
        Text(text = "pigeon-crypto Ed25519 key: $cryptoStatus")
    }
}