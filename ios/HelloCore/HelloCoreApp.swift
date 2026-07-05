// Hello-core — the iOS smoke app (M5.2), the mirror of Android's M0.4 MainActivity.
//
// It installs a log sink and renders two values computed in Rust (`coreVersion()`
// + `selfTestCrypto()`) through the generated Swift bindings, proving the
// Rust → xcframework → UniFFI → Swift → SwiftUI pipeline round-trips on a device.
//
// ⚠ Not compiled in the Linux dev container (no Xcode/Swift). This is reference
// source for the macOS build: create an iOS App target in Xcode, add the
// `PigeonCore` Swift package (../PigeonCore) as a dependency, and add these files.
// Every call here matches the generated `pigeon_mobile_core.swift` signatures.
import SwiftUI
import os
import PigeonCore

@main
struct HelloCoreApp: App {
    init() {
        // Install the host log sink once at launch (the M0.7 callback), like
        // Android's PigeonApp does — it forwards core logs to the platform logger.
        setLogSink(sink: OsLogSink())
        emitTestLog(message: "HelloCore: log sink installed")
    }

    var body: some Scene {
        WindowGroup {
            ContentView()
        }
    }
}

/// Forwards core log records to Apple's unified logging (`os_log`). Never logs
/// message plaintext / tokens / keys (CLAUDE.md Gotcha #2) — the core only emits
/// structured, content-free records.
final class OsLogSink: LogSink {
    private let log = os.Logger(subsystem: "com.pigeon.mobile", category: "core")

    func log(level: LogLevel, target: String, message: String) {
        let line = "\(target): \(message)"
        switch level {
        case .error: log.error("\(line, privacy: .public)")
        case .warn: log.warning("\(line, privacy: .public)")
        case .info, .debug, .trace: log.info("\(line, privacy: .public)")
        }
    }
}
