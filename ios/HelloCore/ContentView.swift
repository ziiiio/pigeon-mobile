// Hello-core screen (M5.2): renders values computed in Rust through the generated
// Swift bindings — the iOS mirror of Android's M0.4 Compose screen.
//
// ⚠ Reference source; compiled on macOS (see ios/README.md). Calls match the
// generated `pigeon_mobile_core.swift`: `coreVersion() -> String` and
// `selfTestCrypto(userId:) throws -> UInt32`.
import SwiftUI
import PigeonCore

struct ContentView: View {
    // core_version() is infallible; self_test_crypto() throws CoreError.
    private let version = coreVersion()
    private let cryptoResult: String = {
        do {
            let keyLen = try selfTestCrypto(userId: "@m5:test.example")
            return "pigeon-crypto Ed25519 key: \(keyLen) bytes"
        } catch {
            return "crypto self-test failed: \(error)"
        }
    }()

    var body: some View {
        VStack(spacing: 16) {
            Text("Pigeon — Hello core")
                .font(.headline)
            Text(version)
                .font(.body.monospaced())
            Text(cryptoResult)
                .font(.body.monospaced())
                .multilineTextAlignment(.center)
        }
        .padding()
    }
}
