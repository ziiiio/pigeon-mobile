// PigeonApp (M5.3) — the iOS app entry, the analogue of Android's `PigeonApp`
// (an `Application`). It installs the core's host callbacks exactly once, at
// process start, before any session op runs:
//
//   • the os_log LogSink (M0.7),
//   • the Keychain-backed KeyStore (M1.3 / Gotcha #1), and
//   • the local store directory — Application Support/PigeonStore, kept out of
//     backups (M2.1/M2.2; see StoreLocation.swift).
//
// Everything downstream (session restore, login, persistence, sync) depends on
// these being in place, so they belong here rather than in a view. This mirrors
// `android/app/.../PigeonApp.kt` one-for-one; the two platforms differ only in
// the OS-integration classes they install.
import SwiftUI
import PigeonCore

@main
struct PigeonApp: App {
    @Environment(\.scenePhase) private var scenePhase

    init() {
        Self.installCoreCallbacks()
    }

    var body: some Scene {
        WindowGroup {
            RootView()
        }
    }

    /// Install the three host callbacks. Idempotent-safe (the core replaces the
    /// installed sink/store), but only ever called once from `init`.
    private static func installCoreCallbacks() {
        setLogSink(sink: OsLogSink())
        setKeyStore(store: KeychainKeyStore())
        // The SQLite store lives in Application Support/PigeonStore/, excluded
        // from iCloud/device backups and file-protected, because it caches the
        // decrypted text of encrypted messages (Gotcha #3). `StoreLocation`
        // also moves an older install's store there. Secrets never land in the
        // store — the token and MLS keys stay in the Keychain (Gotcha #1).
        let appSupport = FileManager.default
            .urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        try? FileManager.default.createDirectory(
            at: appSupport, withIntermediateDirectories: true)
        let sink = OsLogSink()
        let storeDir = StoreLocation.prepare(base: appSupport) { level, message in
            sink.log(level: level, target: "store", message: message)
        }
        setStoreDir(dir: storeDir.path)
        emitTestLog(message: "PigeonApp: core callbacks installed")
    }
}
