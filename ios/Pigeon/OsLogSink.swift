// OsLogSink (M5.3) — the iOS analogue of Android's `LogcatSink`.
//
// The core never assumes a platform logger; the host installs a sink (M0.7 /
// CLAUDE.md "Logging") and the core emits structured records to it. Here we
// forward them to Apple's unified logging (`os_log`).
//
// Content discipline (CLAUDE.md Gotcha #2): the core only emits content-free
// records — never message plaintext, user handles in clear, tokens, or keys —
// so forwarding at `.public` is safe. Do not widen what the core logs.
import os
import PigeonCore

/// Forwards core log records to `os_log` under the `com.pigeon.mobile` subsystem.
/// Installed once at launch by `PigeonApp` via `setLogSink`.
final class OsLogSink: LogSink {
    private let log = Logger(subsystem: "com.pigeon.mobile", category: "core")

    func log(level: LogLevel, target: String, message: String) {
        let line = "\(target): \(message)"
        switch level {
        case .error: log.error("\(line, privacy: .public)")
        case .warn: log.warning("\(line, privacy: .public)")
        case .info: log.info("\(line, privacy: .public)")
        case .debug, .trace: log.debug("\(line, privacy: .public)")
        }
    }
}
